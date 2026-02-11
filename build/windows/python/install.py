from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


def run(cmd: list[str], cwd: Path) -> None:
    print(">", " ".join(cmd))
    subprocess.check_call(cmd, cwd=str(cwd))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Install BetterClock Python client dependencies for this interpreter."
    )
    parser.add_argument(
        "--no-mdns",
        action="store_true",
        help="Install without optional mDNS support (zeroconf).",
    )
    parser.add_argument(
        "--no-editable",
        action="store_true",
        help="Install as a regular package instead of editable mode.",
    )
    parser.add_argument(
        "--upgrade-pip",
        action="store_true",
        help="Upgrade pip before package install.",
    )
    args = parser.parse_args()

    project_dir = Path(__file__).resolve().parent
    pyproject_file = project_dir / "pyproject.toml"
    if not pyproject_file.exists():
        print(f"pyproject.toml not found at {pyproject_file}")
        return 1

    python_exe = sys.executable
    print(f"Python: {python_exe}")
    print(f"Project: {project_dir}")

    if args.upgrade_pip:
        run([python_exe, "-m", "pip", "install", "--upgrade", "pip"], cwd=project_dir)

    target = "." if args.no_mdns else ".[mdns]"
    install_cmd = [python_exe, "-m", "pip", "install"]
    if not args.no_editable:
        install_cmd += ["-e", target]
    else:
        install_cmd += [target]

    run(install_cmd, cwd=project_dir)
    print("Install complete.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
