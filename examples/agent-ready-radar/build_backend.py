"""Minimal standard-library PEP 517 wheel/sdist builder, no build dependencies."""
import base64
import csv
import hashlib
import io
from pathlib import Path
import tarfile
import zipfile

NAME = "agent_ready_radar-0.1.0"
INFO = NAME + ".dist-info"


def metadata():
    return ("Metadata-Version: 2.1\nName: agent-ready-radar\nVersion: 0.1.0\n"
            "Summary: Conservative web-agent surface scanner\nRequires-Python: >=3.12\n"
            "License: MIT\nDescription-Content-Type: text/markdown\n\n" +
            Path("README.md").read_text()).encode()


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    filename = NAME + "-py3-none-any.whl"
    entries = {str(p.relative_to("src")): p.read_bytes() for p in sorted(Path("src").rglob("*.py"))}
    entries.update({INFO + "/METADATA": metadata(),
                    INFO + "/WHEEL": b"Wheel-Version: 1.0\nGenerator: agent-ready-radar\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
                    INFO + "/entry_points.txt": b"[console_scripts]\nagent-ready-radar = agent_ready_radar.__main__:main\n",
                    INFO + "/licenses/LICENSE": Path("LICENSE").read_bytes()})
    rows = []
    for name, value in entries.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(value).digest()).rstrip(b"=").decode()
        rows.append((name, "sha256=" + digest, len(value)))
    output = io.StringIO()
    csv.writer(output, lineterminator="\n").writerows(rows + [(INFO + "/RECORD", "", "")])
    entries[INFO + "/RECORD"] = output.getvalue().encode()
    with zipfile.ZipFile(Path(wheel_directory)/filename, "w", zipfile.ZIP_DEFLATED) as wheel:
        for name, value in entries.items():
            wheel.writestr(name, value)
    return filename


def build_sdist(sdist_directory, config_settings=None):
    filename = "agent_ready_radar-0.1.0.tar.gz"
    paths = [Path(p) for p in ("pyproject.toml", "build_backend.py", "README.md", "LICENSE",
                             "SURVEY_2026_11_17.md", "benchmarks/cohorts.json",
                             "benchmarks/survey-2026-11-17-summary.json",
                             "benchmarks/home-agent-ready-radar-widget.json")]
    paths += sorted(Path("src").rglob("*.py")) + sorted(Path("tests").rglob("*.py")) + sorted(Path("tests").rglob("*.json"))
    with tarfile.open(Path(sdist_directory)/filename, "w:gz") as archive:
        for path in paths:
            archive.add(path, arcname=NAME + "/" + str(path))
    return filename
