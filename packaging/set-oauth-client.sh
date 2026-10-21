#!/usr/bin/env bash
# Stores the CloudDrop OAuth clients for ready-to-use builds (PPA, snap, Windows,
# local installs): Google's client, and the OneDrive and Dropbox client ids. The file
# is gitignored, so the public repository never contains it, but it is packed into
# the PPA source package so Launchpad builds include the clients.
#
#   ./packaging/set-oauth-client.sh ~/Downloads/client_secret_….json
#   ./packaging/set-oauth-client.sh --onedrive CLIENT_ID --dropbox APP_KEY
#
# Each run updates what it is given and keeps the rest of the file.
set -euo pipefail
out="$(dirname "$0")/oauth-client.env"
[ $# -gt 0 ] || { sed -n '7,8s/^#  //p' "$0"; exit 2; }
umask 077
python3 - "$out" "$@" <<'PY'
import json, os, sys
out, args = sys.argv[1], sys.argv[2:]
env = {}
if os.path.exists(out):
    for line in open(out):
        k, sep, v = line.strip().partition("=")
        if sep:
            env[k] = v
clients = dict(kv.split("=", 1) for kv in env.get("CLOUDDROP_OAUTH_CLIENTS", "").split(",") if "=" in kv)
said = []
while args:
    a = args.pop(0)
    if a in ("--onedrive", "--dropbox"):
        if not args or not args[0].strip() or args[0].startswith("--"):
            sys.exit(f"{a} needs a value")
        clients[a[2:]] = args.pop(0).strip()
        said.append(f"{a[2:]} client id")
    else:
        data = json.load(open(a))
        c = data.get("installed")
        if not c or not c.get("client_id") or not c.get("client_secret"):
            sys.exit(f"{a}: not a Google 'Desktop app' client JSON file")
        env["CLOUDDROP_CLIENT_ID"], env["CLOUDDROP_CLIENT_SECRET"] = c["client_id"], c["client_secret"]
        said.append(f"Google client {c['client_id'].split('-')[0]}… (project {c.get('project_id')})")
if clients:
    env["CLOUDDROP_OAUTH_CLIENTS"] = ",".join(f"{k}={v}" for k, v in sorted(clients.items()))
with open(out, "w") as f:
    f.writelines(f"{k}={v}\n" for k, v in env.items())
print(f"saved {', '.join(said)} to {out}")
have = ["Google" if env.get("CLOUDDROP_CLIENT_ID") else None, *sorted(clients)]
print("the file now has:", ", ".join(h for h in have if h) or "nothing")
PY
