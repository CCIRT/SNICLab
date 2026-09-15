#!/usr/bin/env bash

# NOTE: bash dependency is required for this script to run properly.

set -euo pipefail

# variables
readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
echo "Script directory: $SCRIPT_DIR"
read -r -d '' COPYRIGHT_HEADER <<'EOF' || true
Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
All rights reserved.

This software is released under the MIT License.
http://opensource.org/licenses/mit-license.php
EOF

# help text
usage()
{
    cat <<EOF
Usage:
    $(basename "$0") <comment-type> <directory> <extension>
Arguments:
    <comment-type>  : Type of comment to use (e.g., '#', '//')
    <directory>     : Directory to search for source files
    <extension>     : File extension to filter source files (e.g., '.sh', '.c', '.cpp')
Examples:
    $(basename "$0") '#' ./src .sh
    $(basename "$0") '//' ./src .cpp
EOF
}

# check arguments\
if [[ $# -ne 3 ]]
then
    usage >&2
    exit 1
fi

# args
readonly COMMENT_TYPE="$1"
readonly TARGET_DIRECTORY="$2"
readonly TARGET_FILE_EXTENSION="$3"

case "$COMMENT_TYPE" in
    "//" | "#")
        ;;
    *)
        echo "Error: Invalid comment type '$COMMENT_TYPE'. Use '#' or '//'." >&2
        exit 1
        ;;
esac

# Determine the scan directory based on whether the target directory is absolute or relative
if [[ ! -d "$TARGET_DIRECTORY" ]]
then
    echo "Error: Directory '$TARGET_DIRECTORY' does not exist." >&2
    exit 1
fi

SCAN_DIRECTORY="$(
    cd -- "$TARGET_DIRECTORY" && pwd -P
)"

if [[ -z "$TARGET_FILE_EXTENSION" ]]
then
    echo "Error: File extension cannot be empty." >&2
    exit 1
fi

if [[ ! -d "$SCAN_DIRECTORY" ]]
then
    echo "Error: Directory '$SCAN_DIRECTORY' does not exist." >&2
    exit 1
fi

COMMENTED_HEADER="$(
    while IFS= read -r line || [[ -n "$line" ]]
    do
        if [[ -z "$line" ]]
        then
            printf '%s\n' "$COMMENT_TYPE"
        else
            printf '%s %s\n' "$COMMENT_TYPE" "$line"
        fi
    done <<< "$COPYRIGHT_HEADER"
)"

COPYRIGHT_SIGNATURE="$(
    printf '%s\n' "$COMMENTED_HEADER" | head -n 1
)"

# walk through the directory and add the copyright header to each target file
find "$SCAN_DIRECTORY" -type f -name "*.${TARGET_FILE_EXTENSION}" -print0 \
    | while IFS= read -r -d '' target_file
    do
        # skip files that already have the copyright header
        if head -n 10 "$target_file" | grep -qFx "$COPYRIGHT_SIGNATURE"
        then
            echo "Skipping $target_file: already has copyright header."
            continue
        fi

        temporary_file="$(mktemp "${target_file}.XXXXXX")"
        {
            printf '%s\n' "$COMMENTED_HEADER"
            printf '\n'
            cat "$target_file"
        } > "$temporary_file"

        # preserve original file permissions
        chmod --reference="$target_file" "$temporary_file" 2>/dev/null || \
            chmod "$(stat -f '%Lp' "$target_file")" "$temporary_file" 2>/dev/null || true

        mv -- "$temporary_file" "$target_file"
        echo "Added copyright header to $target_file."
    done
