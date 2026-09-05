#!/bin/sh
# Run as PID 1 in the same disposable/private-cgroup setup as verify-isolation.sh.
# Mount a Linux Bun binary at /proof/bun.
set -eu
. /workspace/e2e/docker/server/verify-isolation.sh
install -m 0755 /proof/bun /usr/local/bin/bun
release=/opt/tako/apps/first/production/releases/v1
printf '%s\n' '{"dependencies":{"is-number":"7.0.0"}}' > "$release/package.json"
chown tako:"$first_group" "$release/package.json"
chmod 0660 "$release/package.json"
install -d -m 0700 -o "$first_group" -g "$first_group" /tmp/bun-proof-home
app_install() {
  setpriv --reuid="$first_group" --regid="$first_group" --groups=tako-app --no-new-privs \
    env HOME=/tmp/bun-proof-home sh -c \
    'cd /opt/tako/apps/first/production/releases/v1; bun install --backend=hardlink'
}
app_install
dependency="$release/node_modules/is-number/index.js"
test "$(stat -c %h "$dependency")" -gt 1
before="$(stat -c '%u:%g:%a:%i' "$dependency")"
su -s /bin/sh tako -c 'sudo -n /usr/local/bin/tako-provision-app first/production v1'
test "$(stat -c '%u:%g:%a:%i' "$dependency")" = "$before"
su -s /bin/sh tako -c "test -s '$dependency' && head -c 1 '$dependency' >/dev/null"
printf '%s\n' '{"dependencies":{"is-number":"6.0.0"}}' > "$release/package.json"
app_install
test "$(/usr/local/bin/bun -e "console.log(require('$release/node_modules/is-number/package.json').version)")" = 6.0.0
su -s /bin/sh tako -c 'sudo -n /usr/local/bin/tako-provision-app first/production v1'
su -s /bin/sh tako -c "test -s '$dependency' && head -c 1 '$dependency' >/dev/null"
setpriv --reuid="$first_group" --regid="$first_group" --groups=tako-app --no-new-privs sh -c \
  'cd /opt/tako/apps/first/production/releases/v1; if rm app.json 2>/dev/null; then exit 1; fi'
# Root manifest hardlinks and service-owned dependency hardlinks remain rejected.
ln "$release/app.json" "$release/manifest-link"
if su -s /bin/sh tako -c 'sudo -n /usr/local/bin/tako-provision-app first/production v1'; then exit 1; fi
rm "$release/manifest-link"
printf protected > "$release/service-file"
chown tako:tako "$release/service-file"
ln "$release/service-file" "$release/service-link"
before="$(stat -c '%u:%g:%a' "$release/service-file")"
if su -s /bin/sh tako -c 'sudo -n /usr/local/bin/tako-provision-app first/production v1'; then exit 1; fi
test "$(stat -c '%u:%g:%a' "$release/service-file")" = "$before"
echo 'PASS: Bun hardlink install/reprovision/reinstall, service dependency read, protected manifest and service-owned hardlink denial'
