"""Collect license and copyright notices from the locked Windows Cargo graph."""

import argparse
from collections import defaultdict
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess


PROJECT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
OUTPUT = PROJECT / "THIRD-PARTY-RUST.txt"
SUPPLEMENT_DIR = PROJECT / "tools" / "rust-license-overrides"
ARCHIVE_LICENSE_ISSUE = "crate-level license text absent from the local crate archive"
LICENSE_NAME = re.compile(
    r"^(?:licen[cs]es?|copying|unlicense|ofl|ufl)(?:[._ -].*)?$", re.I
)
NOTICE_NAME = re.compile(
    r"^(?:copyrights?|notice|third[._ -]?party[._ -]?(?:notices?|licenses?))"
    r"(?:[._ -].*)?$", re.I
)
ATTRIBUTION = re.compile(
    r"copyright|public domain|licensed under|governed by|spdx-license-identifier", re.I
)
SOURCE_HEADER = re.compile(r"\A(?:(?:\s*/\*.*?\*/)|(?:\s*//(?![!/])[^\n]*))*", re.S)
README_SECTION = re.compile(r"^(#{1,6})\s+(?:licen[cs]es?|copyright)\b[^\n]*", re.I | re.M)
MARKDOWN_HEADING = re.compile(r"^(#{1,6})[ \t]+", re.M)
README_ATTRIBUTION = re.compile(r"licensed under|copyright (?:\(c\)|©|\d)|public domain", re.I)
IGNORE_DIRS = {".git", ".github", "benches", "examples", "target", "test", "tests"}


def cargo_metadata():
    cargo = shutil.which("cargo") or Path.home() / ".cargo" / "bin" / "cargo.exe"
    command = [
        str(cargo), "metadata", "--format-version", "1", "--locked", "--offline",
        "--filter-platform", TARGET,
    ]
    return json.loads(subprocess.check_output(command, cwd=PROJECT))


def dependencies(metadata):
    packages = {item["id"]: item for item in metadata["packages"]}
    graph = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    pending = [root]
    seen = {root}
    for package_id in pending:
        for dependency in graph[package_id]["deps"]:
            child = dependency["pkg"]
            if child not in seen and any(edge["kind"] is None for edge in dependency["dep_kinds"]):
                pending.append(child)
                seen.add(child)
    return sorted((packages[item] for item in pending[1:]), key=lambda p: (p["name"], p["version"]))


def read_text(path, limit=None):
    if limit is None:
        data = path.read_bytes()
    else:
        with path.open("rb") as stream:
            data = stream.read(limit)
    encoding = "utf-16" if data.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig"
    return data.decode(encoding).replace("\r\n", "\n").replace("\r", "\n").strip()


def collect_files(package):
    folder = Path(package["manifest_path"]).parent.resolve()
    declared_path = None
    if package["license_file"]:
        declared_path = (folder / package["license_file"]).resolve()
    license_text = False
    entries = []
    issues = []
    readme = None
    source_files = []
    if declared_path is not None and (
        not declared_path.is_relative_to(folder) or not declared_path.is_file()
    ):
        issues.append(f"declared license_file is unavailable in the crate archive: {package['license_file']}")

    for directory, subdirs, filenames in os.walk(folder):
        subdirs[:] = sorted(name for name in subdirs if name.casefold() not in IGNORE_DIRS)
        for filename in sorted(filenames):
            path = Path(directory) / filename
            relative = path.relative_to(folder)
            parts = [part.casefold() for part in relative.parts[:-1]]
            if path.suffix == ".rs":
                source_files.append(path)
                continue
            if not parts and filename.casefold().startswith("readme"):
                readme = path
            declared = path == declared_path
            in_licenses = any(part in {"license", "licenses", "licences"} for part in parts)
            font_notice = "fonts" in parts and path.suffix.casefold() in {".txt", ".md"}
            is_license = bool(LICENSE_NAME.fullmatch(filename)) or in_licenses or declared
            is_notice = bool(NOTICE_NAME.fullmatch(filename)) or font_notice
            if not (is_license or is_notice):
                continue
            try:
                content = read_text(path)
            except (OSError, UnicodeError):
                issues.append(f"could not read notice as text: {relative.as_posix()}")
                continue
            if content:
                entries.append((relative.as_posix(), content))
                if is_license and (not parts or in_licenses or declared):
                    license_text = True

    if not license_text:
        issues.append(ARCHIVE_LICENSE_ISSUE)
        if readme is not None:
            try:
                content = read_text(readme)
            except (OSError, UnicodeError):
                issues.append(f"could not read README attribution: {readme.name}")
            else:
                heading = README_SECTION.search(content)
                if heading:
                    end = next((
                        match.start() for match in MARKDOWN_HEADING.finditer(content, heading.end())
                        if len(match[1]) <= len(heading[1])
                    ), len(content))
                    entries.append((f"{readme.name} (license section)", content[heading.start():end].strip()))
                else:
                    excerpts = [line for line in content.splitlines() if README_ATTRIBUTION.search(line)]
                    if excerpts:
                        entries.append((f"{readme.name} (license statements)", "\n".join(excerpts)))
        for path in source_files:
            try:
                header = SOURCE_HEADER.match(read_text(path, 8192))[0].strip()
            except (OSError, UnicodeError):
                continue
            if ATTRIBUTION.search(header):
                entries.append((f"{path.relative_to(folder).as_posix()} (source header)", header))
    if not package["license"]:
        issues.append("SPDX expression absent from Cargo metadata")
    return entries, issues


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_revision(package):
    path = Path(package["manifest_path"]).with_name(".cargo_vcs_info.json")
    try:
        revision = json.loads(path.read_bytes()).get("git", {}).get("sha1", "")
    except (OSError, ValueError):
        return None
    return revision if isinstance(revision, str) and re.fullmatch(r"[0-9a-f]{40}", revision) else None


def upstream_supplements():
    manifest = json.loads((SUPPLEMENT_DIR / "manifest.json").read_bytes())
    index = {}
    for group in manifest:
        revision = group["revision"]
        sources = []
        for source in group["files"]:
            name = source["file"]
            if Path(name).name != name or f"/{revision}/" not in source["url"]:
                raise ValueError(f"Invalid upstream license mapping: {name}")
            path = SUPPLEMENT_DIR / name
            data = path.read_bytes()
            if digest(data) != source["sha256"]:
                raise ValueError(f"Checksum mismatch for upstream license snapshot: {name}")
            sources.append((source, read_text(path)))
        for name, version in group["packages"].items():
            index[(name, version)] = (revision, sources)
    return index


def text_lines(metadata):
    packages = dependencies(metadata)
    supplements = upstream_supplements()
    notices = {}
    users = defaultdict(set)
    rows = []
    omissions = []
    used_supplements = {}
    for package in packages:
        key = f"{package['name']} {package['version']}"
        entries, issues = collect_files(package)
        supplement = supplements.get((package["name"], package["version"]))
        if supplement:
            revision, sources = supplement
            if source_revision(package) != revision:
                issues.append("local crate source revision does not match the upstream supplement")
            else:
                for source, content in sources:
                    name = source["file"]
                    entries.append((f"{name} (pinned upstream snapshot)", content))
                    used_supplements.setdefault(name, (source, []))[1].append(key)
                issues = [issue for issue in issues if issue != ARCHIVE_LICENSE_ISSUE]
        by_text = defaultdict(list)
        for path, content in entries:
            identifier = digest(content.encode("utf-8"))
            notices[identifier] = content
            users[identifier].add(key)
            by_text[identifier].append(path)
        rows.append((key, package["license"] or "(none declared)", by_text))
        if issues:
            omissions.append((key, package.get("repository"), source_revision(package), issues))
    labels = {identifier: f"T{index + 1:03}" for index, identifier in enumerate(sorted(notices))}

    yield "THIRD-PARTY NOTICES FOR WINDOWS X64 RUST DEPENDENCIES"
    yield ""
    yield f"Target: {TARGET}"
    yield f"Cargo.lock SHA-256: {digest((PROJECT / 'Cargo.lock').read_bytes())}"
    yield f"{len(packages)} normal dependencies and {len(notices)} unique notice texts"
    yield "Scope: the locked Cargo graph for this target with the project's default features."
    yield "Development-only and build-only dependency edges are outside this inventory."
    yield "Cargo license declarations are reproduced as supplied. This file assigns no license to this application."
    yield "Notice text is taken from locally unpacked crates and verified upstream snapshots."
    yield "Line endings are normalized."
    yield "Identical texts share a reference. Unavailable texts are listed below."
    yield "Models, native runtime DLLs and Umi-OCR assets have separate notices in NOTICE.md."
    yield ""
    yield "UPSTREAM LICENSE SUPPLEMENTS"
    for name, (source, keys) in sorted(used_supplements.items()):
        yield f"- {name}: {', '.join(keys)}"
        yield f"  Source: {source['url']}"
        yield f"  Source file SHA-256: {source['sha256']}"
    yield ""
    yield "PACKAGES WITH TEXT OR METADATA OMISSIONS"
    if not omissions:
        yield "(none)"
    for key, repository, revision, issues in omissions:
        yield f"- {key}: {'; '.join(issues)}"
        if repository:
            yield f"  Upstream: {repository}"
        if revision:
            yield f"  Packaged source revision: {revision}"
    yield ""
    yield "PACKAGE INDEX"
    for key, license_expression, texts in rows:
        yield f"- {key}: {license_expression}"
        if not texts:
            yield "  No local license or attribution text extracted."
        for identifier, paths in sorted(texts.items(), key=lambda item: labels[item[0]]):
            sample = ", ".join(paths[:3])
            remaining = f" and {len(paths) - 3} more files" if len(paths) > 3 else ""
            yield f"  {labels[identifier]}: {sample}{remaining}"
    yield ""
    yield "DEDUPLICATED LICENSE AND COPYRIGHT TEXTS"
    for identifier in sorted(notices):
        yield ""
        yield f"----- {labels[identifier]} -----"
        yield "Applies to: " + ", ".join(sorted(users[identifier]))
        yield ""
        yield notices[identifier]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--check", action="store_true", help="Verify the existing bundle without writing")
    args = parser.parse_args()
    data = ("\n".join(text_lines(cargo_metadata())) + "\n").encode("utf-8")
    if args.check:
        if not args.output.is_file() or args.output.read_bytes().replace(b"\r\n", b"\n") != data:
            parser.exit(1, f"{args.output.name} does not match the locked local Cargo graph\n")
        print(f"{args.output.name} verified")
    else:
        temporary = args.output.with_suffix(args.output.suffix + ".tmp")
        temporary.write_bytes(data)
        temporary.replace(args.output)
        print(f"{args.output.name} generated ({len(data):,} bytes)")


if __name__ == "__main__":
    main()
