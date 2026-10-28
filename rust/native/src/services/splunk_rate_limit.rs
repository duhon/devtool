use crate::core::error::{Error, Result};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(super) const INTERVAL: Duration = Duration::from_secs(13);
pub(super) const API_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) fn remaining(deadline: Instant) -> Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(Error::new(format!(
            "Splunk search timed out after {} seconds, including API rate-limit waits",
            services_contracts::SPLUNK_TIMEOUT_SECONDS
        )))
    } else {
        Ok(remaining)
    }
}

fn now() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::new(format!("Invalid system clock: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|_| Error::new("System clock exceeds Splunk rate-limit range"))
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

pub(super) struct RateLimit {
    path: PathBuf,
}

impl RateLimit {
    pub(super) fn new(directory: &Path, origin: &str) -> Self {
        // A hash collision only makes two servers share the stricter pacing.
        let hash = origin.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        Self {
            path: directory.join(format!("{hash:016x}.json")),
        }
    }

    pub(super) async fn acquire(&self, deadline: Instant) -> Result<Permit> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = private_file(&self.path.with_extension("lock"))?;
        loop {
            let remaining = remaining(deadline)?;
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(100).min(remaining)).await;
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        let permit = Permit {
            _lock: lock,
            path: self.path.clone(),
            active: false,
            cooldown: INTERVAL,
        };
        let next: u64 = match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| Error::new(format!("Invalid Splunk rate-limit state: {error}")))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let delay = Duration::from_millis(next.saturating_sub(now()?));
        tokio::time::sleep(delay.min(remaining(deadline)?)).await;
        remaining(deadline)?;
        Ok(permit)
    }
}

pub(super) struct Permit {
    _lock: File,
    path: PathBuf,
    active: bool,
    cooldown: Duration,
}

impl Permit {
    fn defer(&self, delay: Duration) -> Result<()> {
        let millis = u64::try_from(delay.as_millis())
            .map_err(|_| Error::new("Splunk Retry-After exceeds the supported range"))?;
        let next = now()?
            .checked_add(millis)
            .and_then(|next| next.checked_add(1))
            .ok_or_else(|| Error::new("Splunk rate-limit timestamp overflow"))?;
        let pending = self
            .path
            .with_extension(format!("{}.pending", std::process::id()));
        let mut file = private_file(&pending)?;
        file.set_len(0)?;
        file.write_all(&serde_json::to_vec(&next)?)?;
        file.sync_all()?;
        fs::rename(pending, &self.path)?;
        Ok(())
    }

    pub(super) fn begin(&mut self, timeout: Duration) -> Result<()> {
        // Keep a conservative reservation if the process exits during an API call.
        self.defer(timeout + INTERVAL)?;
        self.active = true;
        Ok(())
    }

    pub(super) fn finish(&mut self, cooldown: Duration) -> Result<()> {
        self.cooldown = cooldown.max(INTERVAL);
        self.defer(self.cooldown)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = self.defer(self.cooldown)
        {
            eprintln!("Failed to persist Splunk API cooldown: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[tokio::test]
    async fn cancellation_releases_the_lock_without_erasing_shared_cooldowns() {
        let directory = Directory(std::env::temp_dir().join(format!(
                    "services-splunk-pacing-{}-{}",
                    std::process::id(),
                    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
                )));
        let limiter = RateLimit::new(&directory.0, "https://splunk.fixture:8089");
        let mut permit = limiter
            .acquire(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let before_begin = now().unwrap();
        permit.begin(API_TIMEOUT).unwrap();
        let reserved: u64 = serde_json::from_slice(&fs::read(&limiter.path).unwrap()).unwrap();
        assert!(reserved >= before_begin + 133_000);
        let before_drop = now().unwrap();
        drop(permit);
        let after_drop = now().unwrap();
        let next: u64 = serde_json::from_slice(&fs::read(&limiter.path).unwrap()).unwrap();
        assert!(next >= before_drop + 13_000 && next <= after_drop + 13_001);

        let before = fs::read(&limiter.path).unwrap();
        let other = RateLimit::new(&directory.0, "https://splunk.fixture:8089");
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                other.acquire(Instant::now() + Duration::from_secs(1))
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&limiter.path).unwrap(), before);

        fs::write(&limiter.path, b"0").unwrap();
        let mut permit = other
            .acquire(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        permit.begin(API_TIMEOUT).unwrap();
        let before_finish = now().unwrap();
        permit.finish(Duration::from_secs(90)).unwrap();
        drop(permit);
        let next: u64 = serde_json::from_slice(&fs::read(&limiter.path).unwrap()).unwrap();
        assert!(next >= before_finish + 90_000);
        fs::write(&limiter.path, b"invalid").unwrap();
        let error = limiter
            .acquire(Instant::now() + Duration::from_secs(1))
            .await
            .err()
            .unwrap();
        assert!(error.message.contains("Invalid Splunk rate-limit state"));
    }
}
