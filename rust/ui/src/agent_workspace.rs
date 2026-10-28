use crate::{
    console,
    document::{self, Position, ResponseRegion},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, PartialEq)]
pub struct EditCommand {
    pub serial: u64,
    pub service: String,
    pub expected: String,
    pub expected_regions: Vec<ResponseRegion>,
    pub from: Position,
    pub to: Position,
    pub text: String,
}
#[derive(Clone)]
struct Check {
    id: String,
    line: usize,
    signature: String,
}
#[derive(Default)]
struct Document {
    source: String,
    regions: Vec<ResponseRegion>,
    revision: u64,
    checks: Vec<Check>,
}
#[derive(Default)]
pub struct Workspace {
    epoch: String,
    identity_error: Option<String>,
    documents: BTreeMap<String, Document>,
    next: u64,
}
impl Workspace {
    pub fn new(epoch: Result<String, String>) -> Self {
        match epoch {
            Ok(epoch) => Self {
                epoch,
                ..Default::default()
            },
            Err(error) => Self {
                identity_error: Some(error),
                ..Default::default()
            },
        }
    }
    pub fn refresh(&mut self, service: &str, source: &str, regions: &[ResponseRegion]) {
        let document = self.documents.entry(service.into()).or_default();
        if document.revision != 0 && document.source == source && document.regions == regions {
            return;
        }
        let previous = std::mem::take(&mut document.checks);
        let lines: Vec<_> = source.split('\n').collect();
        let candidates: Vec<_> = document::check_lines(source, regions)
            .into_iter()
            .map(|line| {
                let body = document::request_at(source, line, regions).expect("check line");
                (line, format!("{}\n{}", lines[line], body.text))
            })
            .collect();
        for (line, signature) in &candidates {
            let old: Vec<_> = previous
                .iter()
                .filter(|check| check.signature == *signature)
                .collect();
            let id = if old.len() == 1
                && candidates.iter().filter(|(_, s)| s == signature).count() == 1
            {
                old[0].id.clone()
            } else {
                self.next += 1;
                format!("{}{service}-check-{}", self.epoch, self.next)
            };
            document.checks.push(Check {
                id,
                line: *line,
                signature: signature.clone(),
            });
        }
        document.source = source.into();
        document.regions = regions.into();
        document.revision += 1;
    }
    fn document(&self, service: &str) -> Result<&Document, String> {
        if let Some(error) = &self.identity_error {
            return Err(error.clone());
        }
        if !console::SERVICES.contains(&service) {
            return Err("Unknown Console service".into());
        }
        self.documents
            .get(service)
            .ok_or("Console document is unavailable".into())
    }
    pub fn revision(&self, service: &str) -> Result<String, String> {
        let revision = self.document(service)?.revision.to_string();
        Ok(if self.epoch.is_empty() {
            revision
        } else {
            format!("{}:{revision}", self.epoch)
        })
    }
    pub fn validate(&self, service: &str, revision: &str) -> Result<(), String> {
        if self.revision(service)? != revision {
            return Err(
                "CONFLICT: document changed; read fresh context before editing or running".into(),
            );
        }
        Ok(())
    }
    pub fn check_line(&self, service: &str, id: &str) -> Result<usize, String> {
        self.document(service)?
            .checks
            .iter()
            .find(|check| check.id == id)
            .map(|check| check.line)
            .ok_or("CONFLICT: check no longer exists; read fresh context".into())
    }
    pub fn context(&self, service: &str, cursor: usize) -> Result<Value, String> {
        self.context_page(service, cursor, 0, 128)
    }
    pub fn context_page(
        &self,
        service: &str,
        cursor: usize,
        offset: usize,
        limit: usize,
    ) -> Result<Value, String> {
        let doc = self.document(service)?;
        let active = document::request_at(&doc.source, cursor, &doc.regions)
            .and_then(|block| doc.checks.iter().find(|check| check.line == block.start));
        Ok(json!({"service":service,"revision":self.revision(service)?,
            "activeCheck":active.map(|check|check.id.as_str()),
            "totalChecks":doc.checks.len(),"checksOffset":offset,
            "checks":doc.checks.iter().skip(offset).take(limit.clamp(1,128)).map(|check| {
                let header=doc.source.split('\n').nth(check.line).unwrap_or("").trim_start();
                let title=header.split_once(char::is_whitespace).map(|(_,title)|title.trim()).unwrap_or("");
                json!({"id":check.id,"line":check.line+1,"title":title.chars().take(200).collect::<String>()})
            }).collect::<Vec<_>>(),
            "request":active.and_then(|check|document::request_at(&doc.source,check.line,&doc.regions))
                .map(|block|page(&block.text,0,16000))}))
    }
    pub fn read_check(
        &self,
        service: &str,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, String> {
        let doc = self.document(service)?;
        let line = self.check_line(service, id)?;
        let block =
            document::request_at(&doc.source, line, &doc.regions).ok_or("Check is unavailable")?;
        Ok(
            json!({"service":service,"checkId":id,"revision":self.revision(service)?,
            "request":page(&block.text,offset,limit)}),
        )
    }
    pub fn response(
        &self,
        service: &str,
        id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, String> {
        let doc = self.document(service)?;
        let line = self.check_line(service, id)?;
        let lines: Vec<_> = doc.source.split('\n').collect();
        let response = doc
            .regions
            .iter()
            .filter(|region| region.request == line)
            .flat_map(|region| {
                lines[region.header.min(lines.len())..region.end.min(lines.len())]
                    .iter()
                    .copied()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if response.is_empty() {
            return Err("This check has no captured response".into());
        }
        Ok(page(&response, offset, limit))
    }
    pub fn edit(
        &self,
        service: &str,
        args: &Value,
        serial: u64,
        create: bool,
    ) -> Result<EditCommand, String> {
        let doc = self.document(service)?;
        self.validate(service, string(args, "revision")?)?;
        let request = args
            .get("request")
            .map(|value| value.as_str().ok_or("request must be a string"))
            .transpose()?;
        let title = args
            .get("title")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "title must be a string".to_owned())
                    .and_then(validate_title)
            })
            .transpose()?;
        if create && (request.is_none() || title.is_none()) {
            return Err("Creating a check requires both title and request".into());
        }
        if request.is_none() && title.is_none() {
            return Err("Editing a check requires title, request, or both".into());
        }
        if let Some(request) = request {
            if request.len() > 128 * 1024 || request.contains('\0') {
                return Err("Request must be at most 128 KiB and contain no NUL characters".into());
            }
            if request.contains("[redacted]")
                || request.lines().any(|line| {
                    console::is_check(line)
                        || matches!(
                            line.split_whitespace()
                                .next()
                                .unwrap_or("")
                                .to_ascii_lowercase()
                                .as_str(),
                            "#response" | "#responce"
                        )
                })
            {
                return Err("Do not insert masked credentials or #check markers. Use variable references; connection blocks are preserved when omitted.".into());
            }
        }
        let lines: Vec<_> = doc.source.split('\n').collect();
        let end = Position {
            line: lines.len() - 1,
            ch: lines.last().unwrap().encode_utf16().count(),
        };
        let origin = if create {
            args.get("checkId")
                .map(|_| string(args, "checkId"))
                .transpose()?
        } else {
            Some(string(args, "checkId")?)
        }
        .map(|id| {
            self.check_line(service, id).and_then(|line| {
                document::request_at(&doc.source, line, &doc.regions)
                    .ok_or("Check is unavailable".into())
            })
        })
        .transpose()?;
        if request.is_none() {
            let block = origin.as_ref().ok_or("Missing original check")?;
            return Ok(EditCommand {
                serial,
                service: service.into(),
                expected: doc.source.clone(),
                expected_regions: doc.regions.clone(),
                from: Position {
                    line: block.start,
                    ch: 0,
                },
                to: Position {
                    line: block.start,
                    ch: lines[block.start].encode_utf16().count(),
                },
                text: format!("#check {}", title.ok_or("Missing title")?),
            });
        }
        let request = request.ok_or("Missing request")?;
        let mut body = String::new();
        if let Some(block) = &origin {
            let old_sections = console::sections(service, &block.text);
            let new_sections = console::sections(service, request);
            for marker in ["endpoint", "headers", "proto", "options", "env"] {
                if !new_sections.contains_key(marker)
                    && let Some(value) = old_sections.get(marker).filter(|value| !value.is_empty())
                {
                    body.push_str(&format!("#{marker}\n{value}\n\n"));
                }
            }
        }
        body.push_str(request.trim());
        console::parse(service, &body)
            .map_err(|error| format!("Invalid Console request: {error}"))?;
        let (from, to, text) = if create {
            let title = title.ok_or("Missing title")?;
            (
                end,
                end,
                format!(
                    "{}#check {title}\n{}\n",
                    if doc.source.is_empty() { "" } else { "\n\n" },
                    body
                ),
            )
        } else {
            let block = origin.ok_or("Missing original check")?;
            let line = block.start;
            let mut text = match title {
                Some(title) => format!("#check {title}\n{body}"),
                None => body,
            };
            if block.end < lines.len() {
                text.push('\n');
            }
            (
                Position {
                    line: line + usize::from(title.is_none()),
                    ch: 0,
                },
                if block.end < lines.len() {
                    Position {
                        line: block.end,
                        ch: 0,
                    }
                } else {
                    end
                },
                text,
            )
        };
        Ok(EditCommand {
            serial,
            service: service.into(),
            expected: doc.source.clone(),
            expected_regions: doc.regions.clone(),
            from,
            to,
            text,
        })
    }
    pub fn retain_edited_id(&mut self, service: &str, id: &str, line: usize) {
        if let Some(doc) = self.documents.get_mut(service)
            && let Some(check) = doc.checks.iter_mut().find(|check| check.line == line)
        {
            check.id = id.into();
        }
    }
}
fn validate_title(title: &str) -> Result<&str, String> {
    let title = title.trim();
    if title.is_empty() || title.len() > 200 || title.contains(['\n', '\r', '\0']) {
        return Err("Check title must be a single non-empty line of at most 200 bytes".into());
    }
    if console::is_check(title) {
        return Err("title must be a plain check name without #check or #request; the application adds #check automatically".into());
    }
    Ok(title)
}
pub fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("Missing {key}"))
}
pub fn page(text: &str, offset: usize, limit: usize) -> Value {
    let length = text.chars().count();
    let limit = limit.clamp(1, 16000);
    let data: String = text.chars().skip(offset).take(limit).collect();
    let end = offset.saturating_add(data.chars().count()).min(length);
    json!({"text":data,"offset":offset,"total":length,"nextOffset":(end<length).then_some(end)})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_follow_unique_moved_checks_and_stale_writes_fail() {
        let mut workspace = Workspace::default();
        workspace.refresh("mysql", "#check A\nselect 1\n#check B\nselect 2", &[]);
        let id = workspace.context("mysql", 0).unwrap()["checks"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        workspace.refresh("mysql", "#check B\nselect 2\n#check A\nselect 1", &[]);
        assert_eq!(workspace.check_line("mysql", &id).unwrap(), 2);
        assert!(workspace.validate("mysql", "1").is_err());
        assert!(workspace.validate("mysql", "2").is_ok());
    }
    #[test]
    fn reloading_a_workspace_invalidates_old_ids_and_revisions() {
        let mut first = Workspace::new(Ok("instance-a".into()));
        let mut second = Workspace::new(Ok("instance-b".into()));
        for workspace in [&mut first, &mut second] {
            workspace.refresh("mysql", "#check A\nselect 1", &[]);
        }
        let old = first.context("mysql", 0).unwrap();
        assert!(
            second
                .validate("mysql", old["revision"].as_str().unwrap())
                .is_err()
        );
        assert!(
            second
                .check_line("mysql", old["activeCheck"].as_str().unwrap())
                .is_err()
        );
        let mut unavailable = Workspace::new(Err("identity unavailable".into()));
        unavailable.refresh("mysql", "#check A\nselect 1", &[]);
        assert!(unavailable.context("mysql", 0).is_err());
    }
    #[test]
    fn edits_preserve_connections_outputs_and_use_utf16_positions() {
        let mut workspace = Workspace::default();
        let source = "#check A\n#endpoint\nmysql://u:p@host/db\n\nselect 1\n#response\nok\n#check B\nselect '😀'";
        workspace.refresh(
            "mysql",
            source,
            &[ResponseRegion {
                request: 0,
                header: 5,
                end: 7,
                table: None,
            }],
        );
        let id = workspace.context("mysql", 0).unwrap()["checks"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let edit = workspace
            .edit(
                "mysql",
                &json!({"revision":"1","checkId":id,"request":"#query\nselect 2"}),
                1,
                false,
            )
            .unwrap();
        assert_eq!(edit.from, Position { line: 1, ch: 0 });
        assert_eq!(edit.to, Position { line: 5, ch: 0 });
        assert!(edit.text.starts_with("#endpoint\nmysql://"));
        assert!(edit.text.ends_with("\n\n#query\nselect 2\n"));
        let create = workspace
            .edit(
                "mysql",
                &json!({"revision":"1","checkId":id,"title":"C","request":"#query\nselect 3"}),
                2,
                true,
            )
            .unwrap();
        assert_eq!(create.from.ch, "select '😀'".encode_utf16().count());
        assert!(
            workspace
                .edit(
                    "mysql",
                    &json!({"revision":"1","checkId":id,"request":"[redacted]"}),
                    1,
                    false
                )
                .is_err()
        );
    }
    #[test]
    fn titles_are_plain_names_and_prefixed_inputs_are_rejected() {
        let mut workspace = Workspace::default();
        workspace.refresh(
            "rest",
            "#check Products\n#endpoint\nGET https://example.invalid/products",
            &[],
        );
        let context = workspace.context("rest", 0).unwrap();
        assert_eq!(context["checks"][0]["title"], "Products");
        let id = context["activeCheck"].as_str().unwrap();
        for title in [
            "#check Products",
            "#CHECK Products",
            "#request Products",
            "#check",
            " \t#check Products",
        ] {
            let args = json!({"revision":"1","checkId":id,"title":title,"request":"#headers\nX-Test: value"});
            for create in [false, true] {
                assert!(
                    workspace
                        .edit("rest", &args, 1, create)
                        .err()
                        .unwrap()
                        .contains("plain check name")
                );
            }
        }
        for payload in [
            json!({}),
            json!({"title":false}),
            json!({"request":false}),
            json!({"title":""}),
            json!({"title":"bad\ntitle"}),
            json!({"title":"я".repeat(101)}),
        ] {
            let mut args = payload;
            args["revision"] = json!("1");
            args["checkId"] = json!(id);
            assert!(workspace.edit("rest", &args, 1, false).is_err());
        }
        let create=workspace.edit("rest",&json!({"revision":"1","checkId":id,"title":"products overview","request":"#headers\nX-Test: value"}),1,true).unwrap();
        assert!(
            create
                .text
                .starts_with("\n\n#check products overview\n#endpoint\n")
        );
        assert!(!create.text.contains("#check #check"));
        assert_eq!(
            validate_title("#checklist review").unwrap(),
            "#checklist review"
        );
        workspace.refresh("rest", "#request Legacy\ninvalid request", &[]);
        assert_eq!(
            workspace.context("rest", 0).unwrap()["checks"][0]["title"],
            "Legacy"
        );
        let context = workspace.context("rest", 0).unwrap();
        assert!(workspace.edit("rest",&json!({"revision":context["revision"],"checkId":context["activeCheck"],"title":"Renamed invalid draft"}),2,false).is_ok());
    }
    #[test]
    fn renaming_preserves_body_response_ownership_and_check_identity() {
        let mut workspace = Workspace::default();
        let old_header = "#check #check products 😀";
        let source = format!(
            "{old_header}\n#endpoint\nGET https://example.invalid/products\n#body\n{{\"id\":123}}\n#response\ncaptured response\n#check Other\nunfinished draft"
        );
        let regions = vec![ResponseRegion {
            request: 0,
            header: 5,
            end: 7,
            table: None,
        }];
        workspace.refresh("rest", &source, &regions);
        let context = workspace.context("rest", 0).unwrap();
        let id = context["activeCheck"].as_str().unwrap().to_owned();
        let edit = workspace
            .edit(
                "rest",
                &json!({"revision":"1","checkId":id,"title":"products overview"}),
                1,
                false,
            )
            .unwrap();
        assert_eq!(edit.from, Position { line: 0, ch: 0 });
        assert_eq!(
            edit.to,
            Position {
                line: 0,
                ch: old_header.encode_utf16().count()
            }
        );
        assert_eq!(edit.text, "#check products overview");
        let renamed = source.replacen(old_header, &edit.text, 1);
        let mut history = document::EditHistory::default();
        history.synchronize(&source, &regions, 1);
        let changed = history
            .apply(
                renamed,
                &[document::TextChange {
                    from: edit.from,
                    to: edit.to,
                    text: vec![edit.text],
                    removed: vec![old_header.into()],
                    origin: Some("+services-agent".into()),
                }],
                2,
                &mut None,
            )
            .unwrap();
        assert_eq!(changed.regions, regions);
        assert_eq!(
            changed.text.split_once('\n').unwrap().1,
            source.split_once('\n').unwrap().1
        );
        workspace.refresh("rest", &changed.text, &changed.regions);
        workspace.retain_edited_id("rest", &id, 0);
        assert_eq!(
            workspace.context("rest", 0).unwrap()["checks"][0]["title"],
            "products overview"
        );
        assert_eq!(workspace.check_line("rest", &id).unwrap(), 0);
        assert_eq!(
            workspace.response("rest", &id, 0, 16000).unwrap()["text"],
            "#response\ncaptured response"
        );
        assert!(
            workspace
                .edit(
                    "rest",
                    &json!({"revision":"1","checkId":id,"title":"Stale rename"}),
                    2,
                    false
                )
                .err()
                .unwrap()
                .contains("CONFLICT")
        );
    }
    #[test]
    fn combined_title_and_request_edits_preserve_connection_and_response_boundary() {
        let mut workspace = Workspace::default();
        let source = "#check Products\n#endpoint\nGET https://example.invalid/products\n#headers\nX-Test: old\n#response\ncaptured";
        workspace.refresh(
            "rest",
            source,
            &[ResponseRegion {
                request: 0,
                header: 5,
                end: 7,
                table: None,
            }],
        );
        let context = workspace.context("rest", 0).unwrap();
        let edit=workspace.edit("rest",&json!({"revision":"1","checkId":context["activeCheck"],"title":"Updated products","request":"#headers\nX-Test: new"}),1,false).unwrap();
        assert_eq!(edit.from, Position { line: 0, ch: 0 });
        assert_eq!(edit.to, Position { line: 5, ch: 0 });
        assert_eq!(
            edit.text,
            "#check Updated products\n#endpoint\nGET https://example.invalid/products\n\n#headers\nX-Test: new\n"
        );
    }
    #[test]
    fn unicode_pages_and_response_ownership_are_bounded() {
        assert_eq!(
            page("a😀б", 1, 1),
            json!({"text":"😀","offset":1,"total":3,"nextOffset":2})
        );
        let mut workspace = Workspace::default();
        workspace.refresh(
            "rest",
            "#check A\nGET /\n#response\n#check not-input\nok",
            &[ResponseRegion {
                request: 0,
                header: 2,
                end: 5,
                table: None,
            }],
        );
        let context = workspace.context("rest", 4).unwrap();
        assert_eq!(context["checks"].as_array().unwrap().len(), 1);
        let id = context["activeCheck"].as_str().unwrap();
        assert_eq!(
            workspace.response("rest", id, 0, 16000).unwrap()["text"],
            "#response\n#check not-input\nok"
        );
    }
}
