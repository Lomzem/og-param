#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    printf 'usage: %s OUTPUT_DIRECTORY [OUTPUT_BASENAME]\n' "$0" >&2
    exit 2
fi

if ! command -v docker >/dev/null 2>&1; then
    printf 'docker is not installed or is not on PATH\n' >&2
    exit 127
fi

script_dir=$(dirname "$(realpath "$0")")
workspace=$(realpath "$script_dir/..")
output_dir=$1
output_name=${2:-og-param}
mkdir -p "$output_dir"
output_dir=$(realpath "$output_dir")
image=${OG_PARAM_ARTIFACT_IMAGE:-og-param-artifacts:local}

docker_command=$(command -v docker)
docker_executable=$(realpath "$docker_command" 2>/dev/null || printf '%s' "$docker_command")
docker_is_windows=0
if [[ ${docker_executable,,} == *.exe ]]; then
    docker_is_windows=1
    if ! command -v wslpath >/dev/null 2>&1; then
        printf 'wslpath is required when using docker.exe from WSL\n' >&2
        exit 127
    fi
    workspace_for_docker=$(wslpath -w "$workspace")
    output_dir_for_docker=$(wslpath -w "$output_dir")
else
    workspace_for_docker=$workspace
    output_dir_for_docker=$output_dir
fi

if [[ ${OG_PARAM_SKIP_DOCKER_BUILD:-0} != 1 ]]; then
    docker build --target artifacts --tag "$image" "$workspace_for_docker"
fi

docker_run=(
    run --rm
    --network none
    --mount "type=bind,source=$output_dir_for_docker,target=/output"
)
if [[ $docker_is_windows == 0 ]]; then
    docker_run+=(
        --env "OG_PARAM_OUTPUT_UID=$(id -u)"
        --env "OG_PARAM_OUTPUT_GID=$(id -g)"
    )
fi
docker_run+=("$image" "$output_name")
docker "${docker_run[@]}"
