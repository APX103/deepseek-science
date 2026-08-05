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

before_hash=''
if [ -n "$production_db" ]; then
  before_hash=$(sqlite3 -readonly "$production_db" ".sha3sum --schema")
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
  after_hash=$(sqlite3 -readonly "$production_db" ".sha3sum --schema")
  if [ "$before_hash" != "$after_hash" ]; then
    printf '%s\n' "ERROR: production dss.db changed during isolated E2E" >&2
    exit 1
  fi
fi

exit "$app_status"
