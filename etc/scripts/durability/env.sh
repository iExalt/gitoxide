# Isolated Git environment for durability probes. Source it; set PROBE_ROOT first.
# Set GIT_BIN_DIR to run a specific Git build; the output pins the version that actually ran.
[ -z "${GIT_BIN_DIR:-}" ] || export PATH=$GIT_BIN_DIR:$PATH
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_QUARANTINE_PATH
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export HOME=$PROBE_ROOT/home XDG_CONFIG_HOME=$PROBE_ROOT/home/.config
mkdir -p "$HOME"
export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@example.com GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@example.com
export GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z'
unset GIT_TEST_FSYNC
