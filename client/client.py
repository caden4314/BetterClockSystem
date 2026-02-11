#!/usr/bin/env python3
import argparse
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


DEFAULT_BOOTSTRAP_BASE = "http://127.0.0.1:8099"
DEFAULT_RUNTIME_ENDPOINT = "/v1/client/code"
DEFAULT_FETCH_TIMEOUT_MS = 3000
DEFAULT_CACHE_PATH = Path(__file__).with_name(".client_runtime_cache.py")
DEFAULT_RUNTIME_UPDATE_CHECK_MS = 4000


def parse_bootstrap_args(argv: list[str]) -> tuple[argparse.Namespace, list[str]]:
    parser = argparse.ArgumentParser(
        description="BetterClock client bootstrap loader (downloads runtime from server)",
        add_help=True,
    )
    parser.add_argument(
        "--bootstrap-base",
        default=DEFAULT_BOOTSTRAP_BASE,
        help="Base URL for bootstrap server, for example http://192.168.1.50:8099",
    )
    parser.add_argument(
        "--runtime-endpoint",
        default=DEFAULT_RUNTIME_ENDPOINT,
        help="Endpoint path used to download runtime code from server",
    )
    parser.add_argument(
        "--bootstrap-timeout-ms",
        type=int,
        default=DEFAULT_FETCH_TIMEOUT_MS,
        help="Bootstrap download timeout in milliseconds",
    )
    parser.add_argument(
        "--cache-path",
        default=str(DEFAULT_CACHE_PATH),
        help="Local cache path for downloaded runtime code",
    )
    parser.add_argument(
        "--no-cache",
        action="store_true",
        help="Do not write/read bootstrap cache file",
    )
    parser.add_argument(
        "--runtime-update-check-ms",
        type=int,
        default=DEFAULT_RUNTIME_UPDATE_CHECK_MS,
        help="How often the runtime checks for code updates from the server (milliseconds)",
    )
    return parser.parse_known_args(argv)


def normalize_runtime_url(base: str, endpoint: str) -> str:
    left = base.rstrip("/")
    right = endpoint if endpoint.startswith("/") else f"/{endpoint}"
    return f"{left}{right}"


def normalize_state_url(base: str) -> str:
    return f"{base.rstrip('/')}/v1/state"


def fetch_runtime_code(runtime_url: str, timeout_ms: int) -> str:
    request = urllib.request.Request(
        runtime_url,
        headers={"Accept": "text/x-python, text/plain"},
        method="GET",
    )
    with urllib.request.urlopen(request, timeout=max(0.1, timeout_ms / 1000.0)) as response:
        payload = response.read().decode("utf-8")
    if not payload.strip():
        raise RuntimeError("server returned empty runtime code")
    return payload


def write_cache(cache_path: Path, code: str) -> None:
    cache_path.parent.mkdir(parents=True, exist_ok=True)
    cache_path.write_text(code, encoding="utf-8")


def read_cache(cache_path: Path) -> str:
    return cache_path.read_text(encoding="utf-8")


def inject_server_arg(runtime_args: list[str], state_url: str) -> list[str]:
    has_server = False
    for idx, token in enumerate(runtime_args):
        if token == "--server":
            has_server = True
            break
        if token.startswith("--server="):
            has_server = True
            break
        if token == "--":
            break
        if idx + 1 < len(runtime_args) and token == "--server":
            has_server = True
            break
    if has_server:
        return runtime_args
    return ["--server", state_url, *runtime_args]


def run_runtime(
    runtime_code: str,
    runtime_args: list[str],
    *,
    runtime_cache_path: str,
    runtime_endpoint_url: str,
    runtime_update_check_ms: int,
) -> int:
    namespace: dict[str, object] = {
        "__name__": "__betterclock_client_runtime__",
        "__file__": runtime_cache_path
        if runtime_cache_path
        else "<betterclock_client_runtime_from_server>",
        "__runtime_source__": runtime_code,
        "__runtime_cache_path__": runtime_cache_path,
        "__runtime_endpoint_url__": runtime_endpoint_url,
        "__runtime_args__": list(runtime_args),
        "__runtime_update_check_ms__": int(max(1000, runtime_update_check_ms)),
        "__runtime_bootstrap_pid__": os.getpid(),
    }
    exec(compile(runtime_code, "<betterclock_client_runtime_from_server>", "exec"), namespace)
    entry = namespace.get("main")
    if not callable(entry):
        raise RuntimeError("runtime code does not expose callable main(argv)")
    result = entry(runtime_args)
    if result is None:
        return 0
    if not isinstance(result, int):
        return 0
    return result


def main(argv: list[str]) -> int:
    bootstrap_args, runtime_args = parse_bootstrap_args(argv)
    runtime_url = normalize_runtime_url(
        bootstrap_args.bootstrap_base, bootstrap_args.runtime_endpoint
    )
    state_url = normalize_state_url(bootstrap_args.bootstrap_base)
    runtime_args = inject_server_arg(runtime_args, state_url)
    cache_path = Path(bootstrap_args.cache_path)

    code: str
    try:
        code = fetch_runtime_code(runtime_url, bootstrap_args.bootstrap_timeout_ms)
        if not bootstrap_args.no_cache:
            write_cache(cache_path, code)
    except Exception as fetch_error:
        if bootstrap_args.no_cache or not cache_path.exists():
            raise RuntimeError(
                f"failed to fetch runtime from {runtime_url}: {fetch_error}"
            ) from fetch_error
        code = read_cache(cache_path)

    runtime_cache_path = "" if bootstrap_args.no_cache else str(cache_path)
    return run_runtime(
        code,
        runtime_args,
        runtime_cache_path=runtime_cache_path,
        runtime_endpoint_url=runtime_url,
        runtime_update_check_ms=bootstrap_args.runtime_update_check_ms,
    )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
