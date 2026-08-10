#!/bin/sh

# Launch a packaged Deepseek Science app against a one-shot private data directory.
# The directory (including any copied credential) is removed when the app exits.

set -eu

usage() {
  printf '%s\n' "Usage: $0 --app /absolute/path/Deepseek\ Science.app [--settings /absolute/path/settings.json]"
}

app_path=''
settings_path=''

while [ "$#" -gt 0 ]; do
  case "$1" in
    --app) app_path=${2-}; shift 2 ;;
    --settings) settings_path=${2-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf '%s\n' "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$app_path" in
  /*.app) ;;
  *) printf '%s\n' "--app must be an absolute .app path" >&2; exit 2 ;;
esac

if [ ! -d "$app_path" ]; then
  printf '%s\n' "App bundle not found: $app_path" >&2
  exit 2
fi

if [ -n "$settings_path" ]; then
  case "$settings_path" in
    /*) ;;
    *) printf '%s\n' "--settings must be an absolute path" >&2; exit 2 ;;
  esac
  if [ ! -f "$settings_path" ]; then
    printf '%s\n' "Settings file not found: $settings_path" >&2
    exit 2
  fi
fi

test_data_dir=$(mktemp -d /private/tmp/deepseek-science-e2e.XXXXXX)
case "$test_data_dir" in
  /private/tmp/deepseek-science-e2e.*) ;;
  *) printf '%s\n' "Refusing unexpected test directory: $test_data_dir" >&2; exit 1 ;;
esac
chmod 700 "$test_data_dir"

cleanup() {
  case "$test_data_dir" in
    /private/tmp/deepseek-science-e2e.*)
      rm -rf -- "$test_data_dir"
      ;;
  esac
}
trap cleanup EXIT HUP INT TERM

if [ -n "$settings_path" ]; then
  install -m 600 "$settings_path" "$test_data_dir/settings.json"
fi

production_db=''
if [ -n "${HOME-}" ] && [ -f "$HOME/.deepseek-science/dss.db" ]; then
  production_db="$HOME/.deepseek-science/dss.db"
fi

production_db_fingerprint() {
  # Normalize a private snapshot before hashing it. Copying the database and
  # WAL keeps production read-only, while SQLite can rebuild SHM state in the
  # snapshot without making WAL checkpoints look like content changes.
  fingerprint_dir=$(mktemp -d "$test_data_dir/production-db-fingerprint.XXXXXX")
  fingerprint_db="$fingerprint_dir/dss.db"
  normalized_db="$fingerprint_dir/normalized.db"
  install -m 600 "$production_db" "$fingerprint_db"
  if [ -f "$production_db-wal" ]; then
    install -m 600 "$production_db-wal" "$fingerprint_db-wal"
  fi

  # The private copy must be writable so SQLite can create its own SHM file and
  # replay a copied WAL. The production files above are still only ever read.
  if ! sqlite3 "$fingerprint_db" "VACUUM INTO '$normalized_db';"; then
    printf '%s\n' "ERROR: could not fingerprint production dss.db" >&2
    return 1
  fi

  (cd "$fingerprint_dir" && shasum -a 256 normalized.db)
}

before_hash=''
if [ -n "$production_db" ]; then
  before_hash=$(production_db_fingerprint)
fi

printf '%s\n' "Isolated DSS_DATA_DIR: $test_data_dir"
if [ -n "$settings_path" ]; then
  printf '%s\n' "Settings copied with mode 0600; credential contents are not printed."
fi

set +e
open -n -W --env "DSS_DATA_DIR=$test_data_dir" "$app_path"
app_status=$?
set -e

if [ -n "$production_db" ]; then
  after_hash=$(production_db_fingerprint)
  if [ "$before_hash" != "$after_hash" ]; then
    printf '%s\n' "ERROR: production dss.db changed during isolated E2E" >&2
    exit 1
  fi
fi

exit "$app_status"
