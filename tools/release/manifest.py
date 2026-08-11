#!/usr/bin/env python3
import hashlib
import json
import os
import re
from pathlib import Path


root = Path(__file__).resolve().parents[2]
dist = Path(os.environ.get("DIST", root / "dist"))
version = (root / "VERSION").read_text().strip()
assets = []

for path in sorted(dist.iterdir()):
    if not path.is_file() or path.name in {"SHA256SUMS", "release-manifest.json"}:
        continue
    lower = path.name.lower()
    platform = next((item for item in ("windows", "linux", "macos", "android") if item in lower), None)
    if not platform:
        continue
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    architecture = "universal"
    for marker, label in (("arm64", "arm64"), ("x86_64", "x86-64"), ("x86", "x86"), ("arm", "arm")):
        if re.search(rf"(?:^|[-_.]){marker}(?:[-_.]|$)", lower):
            architecture = label
            break
    assets.append(
        {
            "name": path.name,
            "platform": platform,
            "architecture": architecture,
            "size": path.stat().st_size,
            "sha256": digest,
        }
    )

(dist / "SHA256SUMS").write_text("".join(f"{item['sha256']}  {item['name']}\n" for item in assets))
(dist / "release-manifest.json").write_text(
    json.dumps({"schema": 1, "version": version, "prerelease": "-" in version, "assets": assets}, indent=2) + "\n"
)
