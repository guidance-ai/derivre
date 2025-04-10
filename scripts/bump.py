#!/usr/bin/env python3

import re
import subprocess
import sys
import os

version_pattern = r'\nversion\s*=\s*"(\d+\.\d+\.\d+)([^"]*)"'


def get_current_version(file_path):
    with open(file_path, "r") as f:
        content = f.read()
    match = re.search(version_pattern, content)
    if match:
        return match.group(1)
    raise ValueError(f"Version not found in {file_path}")


def bump_patch_version(version: str):
    major, minor, patch = map(int, version.split("."))
    patch += 1
    return f"{major}.{minor}.{patch}"


def update_version_in_file(file_path, new_version):
    with open(file_path, "r") as f:
        content = f.read()

    new_content = re.sub(version_pattern, f'\nversion = "{new_version}"',
                         content)

    with open(file_path, "w") as f:
        f.write(new_content + "\n[workspace]\n")

    subprocess.run(["cargo", "check"], check=True)

    with open(file_path, "w") as f:
        f.write(new_content)


def check_in_and_tag(version):
    subprocess.run(["git", "add", "Cargo.toml", "Cargo.lock"],
                   check=True)
    subprocess.run(["git", "commit", "-m", f"Bump version to {version}"],
                   check=True)
    subprocess.run(["git", "tag", f"v{version}"], check=True)
    subprocess.run(["git", "push"], check=True)
    subprocess.run(["git", "push", "--tags"], check=True)


def ensure_clean_working_tree():
    status_output = subprocess.run(["git", "status", "--porcelain"],
                                   capture_output=True,
                                   text=True).stdout
    if status_output.strip():
        subprocess.run(["git", "status"])
        print(
            "\n\nWorking tree is not clean. Please commit or stash your changes before running this script.\n"
        )
        sys.exit(1)


def main():
    ensure_clean_working_tree()

    current_version = get_current_version("Cargo.toml")
    suggested_version = bump_patch_version(current_version)

    print(f"Current version: {current_version}")
    new_version = (input(f"Enter new version (default: {suggested_version}): ")
                   or suggested_version)

    update_version_in_file("Cargo.toml", new_version)

    check_in_and_tag(new_version)


if __name__ == "__main__":
    main()
