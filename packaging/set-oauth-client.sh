#!/usr/bin/env bash
# Stores the CloudDrop OAuth client for ready-to-use builds (PPA, local installs).
# The file is gitignored, so the public repository never contains it, but it is
# packed into the PPA source package so Launchpad builds include the client.
#
#   ./packaging/set-oauth-client.sh ~/Downloads/client_secret_….json
set -euo pipefail
json=${1:?usage: $0 CLIENT_SECRET_JSON}
out="$(dirname "$0")/oauth-client.env"
umask 077
python3 - "$json" "$out" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
c = data.get("installed")
if not c or not c.get("client_id") or not c.get("client_secret"):
    sys.exit("not a Google 'Desktop app' client JSON file")
with open(sys.argv[2], "w") as f:
    f.write(f"CLOUDDROP_CLIENT_ID={c['client_id']}\n")
    f.write(f"CLOUDDROP_CLIENT_SECRET={c['client_secret']}\n")
print(f"saved client {c['client_id'].split('-')[0]}… (project {c.get('project_id')}) to {sys.argv[2]}")
PY
