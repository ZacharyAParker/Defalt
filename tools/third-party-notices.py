"""Write THIRD-PARTY-NOTICES.md from what the project actually uses.

    Rust crates        `cargo metadata` over Cargo.lock (Windows target)
    Python packages    requirements.lock, licenses read from the installed
                       packages (run it with the venv's python)
    fonts, vendored    assets/fonts, web/static/fonts, vendor/README.md
    license texts      tools/licenses/ and the OFL files next to the fonts

Nothing here is typed by hand per package, so the list can't drift from the
lock files. Build once first so cargo has every crate's manifest cached (it
runs offline), then:

    .venv\\Scripts\\python.exe tools\\third-party-notices.py
    .venv\\Scripts\\python.exe tools\\third-party-notices.py --check

The same inputs always give the same file: everything is sorted and nothing
depends on the date or the machine.
"""
from __future__ import annotations

import argparse
import importlib.metadata as metadata
import json
import os
import re
import shutil
import subprocess
import sys
import tomllib
from collections import deque
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "THIRD-PARTY-NOTICES.md"
LICENSES = ROOT / "tools" / "licenses"
TARGET = "x86_64-pc-windows-msvc"
COPYRIGHT = "© 2026 Zachary Parker"

# Full texts included once each. Everything else gets a link.
FULL_TEXTS = {
    "MIT": ("MIT License", LICENSES / "MIT.txt"),
    "Apache-2.0": ("Apache License 2.0", LICENSES / "Apache-2.0.txt"),
    "BSD-3-Clause": ("BSD 3-Clause License", LICENSES / "BSD-3-Clause.txt"),
    "BSD-2-Clause": ("BSD 2-Clause License", LICENSES / "BSD-2-Clause.txt"),
    "ISC": ("ISC License", LICENSES / "ISC.txt"),
    "Zlib": ("zlib License", LICENSES / "Zlib.txt"),
    "OFL-1.1": ("SIL Open Font License 1.1", ROOT / "assets" / "fonts" / "OFL-Archivo.txt"),
}

# Python metadata says the same thing a dozen ways.
PYTHON_ALIASES = {
    "apache 2.0": "Apache-2.0",
    "apache license 2.0": "Apache-2.0",
    "apache software license": "Apache-2.0",
    "mit license": "MIT",
    "bsd license": "BSD (see package)",
    "python software foundation license": "PSF-2.0",
    "gnu lesser general public license v3 (lgplv3)": "LGPL-3.0",
    "mozilla public license 2.0 (mpl 2.0)": "MPL-2.0",
    "the unlicense (unlicense)": "Unlicense",
}

# Invoked by Defalt when you have them installed; never shipped with it.
EXTERNAL_TOOLS = [
    ("FFmpeg and ffprobe", "LGPL-2.1-or-later, or GPL-2.0-or-later for builds that enable GPL components",
     "https://ffmpeg.org/legal.html", "decodes, measures and converts audio"),
    ("cloudflared", "Apache-2.0", "https://github.com/cloudflare/cloudflared",
     "runs the optional remote-listening tunnel"),
    ("Codex CLI", "Apache-2.0", "https://github.com/openai/codex",
     "optional writing backend, signed in separately"),
    ("Python", "PSF-2.0", "https://docs.python.org/3/license.html",
     "runs the radio backend"),
]

# egui's default_fonts feature compiles these into the console.
EGUI_FONTS = [
    ("Ubuntu Light", "Ubuntu-font-1.0", "© Canonical Ltd."),
    ("Hack", "MIT (with the Bitstream Vera license for inherited glyphs)", "© Source Foundry Authors"),
    ("Noto Emoji", "OFL-1.1", "© Google Inc."),
    ("emoji-icon-font", "MIT", "© John Slegers"),
]


def unwrap(text: str) -> list[str]:
    """One line per paragraph, which is how the in-app reader shows text."""
    paragraphs, current = [], []
    for raw in text.replace("\r\n", "\n").split("\n"):
        line = raw.strip()
        if set(line) <= {"-"}:  # the OFL's rules between sections
            line = ""
        if not line:
            if current:
                paragraphs.append(" ".join(current))
                current = []
            continue
        current.append(line)
    if current:
        paragraphs.append(" ".join(current))
    return paragraphs


def people(names) -> str:
    """Authors without their email addresses, in the order given."""
    cleaned = []
    for name in names:
        name = re.sub(r"<[^>]*>?|\S+@\S+", "", name or "").strip().strip('"').strip()
        if name and name not in cleaned:
            cleaned.append(name)
    return ", ".join(cleaned)


def spdx_ids(expression: str) -> set[str]:
    """License identifiers in an expression, without the operators or prose."""
    return {token for token in re.split(r"[\s()/]+", expression)
            if token and token not in {"AND", "OR", "WITH", "BSD"}
            and (token[0].isupper() or token[0].isdigit())
            and re.fullmatch(r"[A-Za-z0-9.+-]+", token)}


def cargo_binary() -> str:
    for candidate in (os.environ.get("CARGO"), shutil.which("cargo"),
                      str(Path.home() / ".cargo" / "bin" / ("cargo.exe" if os.name == "nt" else "cargo"))):
        if candidate and Path(candidate).exists():
            return candidate
    raise FileNotFoundError("cargo not found; install Rust or set CARGO")


def cargo_metadata() -> dict:
    output = subprocess.run(
        [cargo_binary(), "metadata", "--format-version", "1", "--locked", "--offline",
         "--filter-platform", TARGET],
        cwd=ROOT, check=True, capture_output=True, text=True, encoding="utf-8")
    return json.loads(output.stdout)


def crates(data: dict) -> dict[str, list[dict]]:
    """Every crate but Defalt itself, by how it is used: shipped, build, dev."""
    nodes = {node["id"]: node for node in data["resolve"]["nodes"]}
    root = data["resolve"]["root"]

    def reach(kinds_at_root: set, kinds_below: set) -> set[str]:
        seen, queue = {root}, deque([root])
        while queue:
            current = queue.popleft()
            allowed = kinds_at_root if current == root else kinds_below
            for dep in nodes[current]["deps"]:
                kinds = {kind["kind"] for kind in dep["dep_kinds"]}
                if kinds & allowed and dep["pkg"] not in seen:
                    seen.add(dep["pkg"])
                    queue.append(dep["pkg"])
        return seen

    shipped = reach({None}, {None})
    built = reach({None, "build"}, {None, "build"})
    every = reach({None, "build", "dev"}, {None, "build"})
    groups = {"shipped": [], "build": [], "dev": []}
    for package in data["packages"]:
        pid = package["id"]
        if pid == root or pid not in every:
            continue
        group = "shipped" if pid in shipped else "build" if pid in built else "dev"
        groups[group].append({
            "name": package["name"],
            "version": package["version"],
            "license": (package.get("license") or "").replace("/", " OR ") or "see package",
            "authors": people(package.get("authors") or []),
        })
    for group in groups.values():
        group.sort(key=lambda p: (p["name"].lower(), p["version"]))
    return groups


def python_license(meta) -> str:
    expression = (meta.get("License-Expression") or "").strip()
    if expression:
        return expression
    declared = (meta.get("License") or "").strip()
    if declared and "\n" not in declared and len(declared) <= 40:
        return PYTHON_ALIASES.get(declared.lower(), declared)
    for classifier in meta.get_all("Classifier") or []:
        if classifier.startswith("License :: "):
            name = classifier.split(" :: ")[-1]
            return PYTHON_ALIASES.get(name.lower(), name)
    return "see package"


def python_packages() -> list[dict]:
    packages = []
    for line in (ROOT / "requirements.lock").read_text(encoding="utf-8").splitlines():
        line = line.split("#")[0].strip()
        if not line:
            continue
        name, version = re.split(r"(?:==|>=)", line, maxsplit=1)
        pinned = "==" in line
        try:
            meta = metadata.metadata(name)
            license, authors = python_license(meta), people([meta.get("Author") or meta.get("Author-email") or ""])
        except metadata.PackageNotFoundError:
            license, authors = "not installed here; see package", ""
        packages.append({"name": name.strip(), "version": version.strip() if pinned else f">={version.strip()} (unpinned)",
                         "license": license, "authors": authors})
    return sorted(packages, key=lambda p: p["name"].lower())


def first_copyright(path: Path) -> str:
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.strip().lower().startswith("copyright"):
            return line.strip()
    return ""


def vendored() -> list[tuple[str, str, str]]:
    readme = (ROOT / "vendor" / "README.md").read_text(encoding="utf-8")
    found = []
    for name, version in re.findall(r"^- (Signalsmith \w+) ([\d.]+)", readme, re.M):
        folder = ROOT / "vendor" / name.lower().replace(" ", "-")
        found.append((f"{name} {version}", first_copyright(folder / "LICENSE.txt"),
                      f"vendor/{folder.name}/LICENSE.txt"))
    return found


def line(entry: dict) -> str:
    who = f" — {entry['authors']}" if entry["authors"] else ""
    return f"- {entry['name']} {entry['version']} — {entry['license']}{who}"


def render(groups: dict, python: list[dict]) -> str:
    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    out = [
        "# Third-party notices",
        "",
        "Generated by tools/third-party-notices.py from Cargo.lock, requirements.lock and the bundled asset folders. Don't edit it by hand; run the script again after changing dependencies.",
        "",
        "Defalt's own code, art and documents are covered by LICENSE. The components below belong to their authors and are used under their own licenses. Nothing in Defalt's LICENSE or TERMS changes those licenses, and where they conflict for a component, that component's license wins.",
        "",
        "## Summary",
        "",
        f"- Rust crates compiled into the console: {len(groups['shipped'])}",
        f"- Rust crates used only to build it: {len(groups['build'])}",
        f"- Rust crates used only by its tests: {len(groups['dev'])}",
        f"- Python packages for the radio backend (installed by you from PyPI, not shipped in this repository): {len(python)}",
        "- Fonts, vendored source and external tools: listed below",
        "",
        "## Fonts",
        "",
    ]
    fonts = [("Archivo", ROOT / "assets" / "fonts" / "OFL-Archivo.txt"),
             ("IBM Plex Mono", ROOT / "assets" / "fonts" / "OFL-IBMPlexMono.txt")]
    for name, path in fonts:
        out.append(f"- {name} — OFL-1.1 — {first_copyright(path).rstrip(".")}. Used by the console and the browser player; license text in assets/fonts/{path.name} and web/static/fonts/{path.name}")
    cargo_toml = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    if "default_fonts" in cargo_toml and any(p["name"] == "epaint_default_fonts" for p in groups["shipped"]):
        for name, license, holder in EGUI_FONTS:
            out.append(f"- {name} — {license} — {holder.rstrip(".")}. Built into the console through egui's default fonts (epaint_default_fonts)")
    out += ["", "## Vendored source", ""]
    for name, holder, path in vendored():
        out.append(f"- {name} — MIT — {holder.rstrip(".")}. Unmodified upstream headers compiled into the console; license text in {path}")
    out += [
        "",
        "## Web assets",
        "",
        "- The browser player bundles the Archivo and IBM Plex Mono fonts listed above. Its scripts, styles, icons and studio artwork are original Defalt material covered by LICENSE; it loads no third-party scripts.",
        "",
        "## Rust crates",
        "",
        "### Compiled into the console",
        "",
    ]
    out += [line(entry) for entry in groups["shipped"]]
    out += ["", "### Build tools only", ""]
    out += [line(entry) for entry in groups["build"]]
    out += ["", "### Tests only", ""]
    out += [line(entry) for entry in groups["dev"]] or ["- none"]
    out += ["", "## Python packages", "",
            "Pinned in requirements.lock and installed with pip into your own environment. They aren't redistributed in this repository or in the console executable.",
            ""]
    out += [line(entry) for entry in python]
    out += ["", "## External tools (not bundled)", "",
            "Defalt calls these when you have installed them yourself. They aren't included in this repository or the executable, and their own licenses and terms apply.",
            ""]
    for name, license, link, role in EXTERNAL_TOOLS:
        out.append(f"- {name} — {license} — {role} — {link}")
    out += ["", "## License texts", "",
            "Each license's text appears once. The copyright line in each template stands for the holders named against each component above; the original license files ship inside each package's source.",
            ""]
    used = set()
    for entry in groups["shipped"] + groups["build"] + groups["dev"] + python:
        used |= spdx_ids(entry["license"])
    used |= {"OFL-1.1", "MIT"}
    for spdx, (title, path) in FULL_TEXTS.items():
        if spdx in used:
            out += [f"### {title}", ""]
            for paragraph in unwrap(path.read_text(encoding="utf-8")):
                # The heading already names the license, and a font's own
                # copyright line is listed with the font above.
                if paragraph.lower() == title.lower() or (spdx == "OFL-1.1" and paragraph.startswith("Copyright")):
                    continue
                out += [paragraph, ""]
    others = sorted(spdx for spdx in used if spdx not in FULL_TEXTS)
    if others:
        out += ["### Other licenses", "",
                "Full texts for the remaining licenses named above ship with the components that use them and are published at:", ""]
        out += [f"- {spdx}: https://spdx.org/licenses/{spdx}.html" for spdx in others]
        out.append("")
    out += ["---", f"Defalt v{version} · {COPYRIGHT}", ""]
    return "\n".join(out)


def generate() -> str:
    return render(crates(cargo_metadata()), python_packages())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the committed file is out of date")
    parser.add_argument("--output", type=Path, default=OUT)
    args = parser.parse_args()
    text = generate()
    if args.check:
        current = args.output.read_text(encoding="utf-8") if args.output.exists() else ""
        if current != text:
            print(f"{args.output.name} is out of date; run tools/third-party-notices.py", file=sys.stderr)
            return 1
        print(f"{args.output.name} is up to date")
        return 0
    args.output.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
