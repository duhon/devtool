#!/bin/sh
# Runs the kubernetes driver tests, then the module end-to-end tests, against a throwaway k3s
# cluster in Docker. Opt-in: the default `cargo test` skips them (KUBERNETES_TEST_KUBECONFIG is
# not set). It never reads ~/.kube or the caller's KUBECONFIG: the tests use the kubeconfig
# files created here.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$root"
work="$root/target/k3s-it"
rm -rf "$work"
mkdir -p "$work"
name="sdt-k3s-$$"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM
docker run -d --name "$name" --privileged -p 127.0.0.1::6443 \
    "${K3S_IMAGE:-rancher/k3s:v1.31.4-k3s1}" server --disable traefik --tls-san 127.0.0.1 >/dev/null
kubectl_in() { docker exec "$name" kubectl "$@"; }
attempt=0
until kubectl_in get serviceaccount default -n default >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 120 ]; then docker logs "$name" | tail -30; exit 1; fi
    sleep 1
done
port=$(docker port "$name" 6443/tcp | head -1 | sed 's/.*://')
docker cp "$name:/etc/rancher/k3s/k3s.yaml" "$work/k3s.yaml"
sed "s#https://127.0.0.1:6443#https://127.0.0.1:$port#" "$work/k3s.yaml" > "$work/kubeconfig"

kubectl_in create serviceaccount exec-admin -n default >/dev/null
kubectl_in create clusterrolebinding exec-admin --clusterrole=cluster-admin \
    --serviceaccount=default:exec-admin >/dev/null
kubectl_in create serviceaccount restricted -n default >/dev/null
kubectl_in create token exec-admin -n default --duration=1h > "$work/exec-token"
restricted=$(kubectl_in create token restricted -n default --duration=1h)

cat > "$work/plugin.sh" <<EOF
#!/bin/sh
printf '{"apiVersion":"client.authentication.k8s.io/v1","kind":"ExecCredential","status":{"token":"%s"}}' "\$(cat "$work/exec-token")"
EOF
chmod +x "$work/plugin.sh"
cat > "$work/extra" <<EOF
apiVersion: v1
kind: Config
users:
- name: exec-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: $work/plugin.sh
      interactiveMode: IfAvailable
- name: restricted-user
  user:
    token: $restricted
contexts:
- name: k3s-exec
  context: {cluster: default, user: exec-user}
- name: k3s-restricted
  context: {cluster: default, user: restricted-user}
EOF

KUBERNETES_TEST_KUBECONFIG="$work/kubeconfig:$work/extra" \
    cargo test --locked -p services-native --test drivers kubernetes_k3s -- --test-threads=1 --nocapture
KUBERNETES_TEST_KUBECONFIG="$work/kubeconfig:$work/extra" \
    cargo test --locked -p services-module-kubernetes --test integration -- --test-threads=1 --nocapture
