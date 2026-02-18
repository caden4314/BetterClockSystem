from __future__ import annotations

import ctypes
import os
import re
import shutil
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Optional


@dataclass(frozen=True)
class FontEntry:
    id: str
    name: str
    path: Path
    extension: str
    source_kind: str
    source_container: Path

    def as_dict(self) -> dict[str, str]:
        return {
            "id": self.id,
            "name": self.name,
            "path": str(self.path),
            "extension": self.extension,
            "source_kind": self.source_kind,
            "source_container": str(self.source_container),
        }


class SegmentedFontLoader:
    """
    Plugin-like loader for segmented fonts.

    Default layout:
    ui/
      font_loader.py
      fonts/
        *.zip
        */
    """

    DEFAULT_SCAN_EXTENSIONS = ("ttf", "otf", "ttc", "woff", "woff2")
    RUNTIME_EXTENSIONS = ("ttf", "otf", "ttc")

    def __init__(
        self,
        fonts_root: Optional[Path | str] = None,
        extracted_zip_cache_dir: str = ".zip_cache",
        auto_extract: bool = True,
    ) -> None:
        module_dir = Path(__file__).resolve().parent
        self.fonts_root = Path(fonts_root) if fonts_root else (module_dir / "fonts")
        self.auto_extract = auto_extract
        self.zip_cache_root = self.fonts_root / extracted_zip_cache_dir
        self._catalog: list[FontEntry] = []
        if self.auto_extract:
            self.extract_all_archives()

    def extract_all_archives(self, force: bool = False) -> list[Path]:
        return self.extract_all_archives_with_progress(force=force, progress_callback=None)

    def extract_all_archives_with_progress(
        self,
        force: bool = False,
        progress_callback: Optional[Callable[[int, int, str], None]] = None,
    ) -> list[Path]:
        archives = sorted(
            p
            for p in self.fonts_root.rglob("*.zip")
            if p.is_file() and not self._is_in_zip_cache(p)
        )
        total = len(archives)
        if progress_callback is not None:
            progress_callback(0, total, "Looking for zip archives")
        if not archives:
            return []
        extracted: list[Path] = []
        self.zip_cache_root.mkdir(parents=True, exist_ok=True)
        for index, archive in enumerate(archives, start=1):
            rel_archive = archive.relative_to(self.fonts_root)
            target_dir = self.zip_cache_root / _safe_slug(str(rel_archive))
            stamp_file = target_dir / ".stamp"
            source_stamp = str(archive.stat().st_mtime_ns)
            should_extract = force or not target_dir.exists() or not stamp_file.exists()
            if not should_extract and stamp_file.exists():
                should_extract = stamp_file.read_text(encoding="utf-8") != source_stamp
            if should_extract:
                if target_dir.exists():
                    shutil.rmtree(target_dir)
                target_dir.mkdir(parents=True, exist_ok=True)
                with zipfile.ZipFile(archive, "r") as zf:
                    zf.extractall(target_dir)
                stamp_file.write_text(source_stamp, encoding="utf-8")
                status = f"Extracted zip {index}/{total}: {archive.name}"
            else:
                status = f"Zip cache up-to-date {index}/{total}: {archive.name}"
            extracted.append(target_dir)
            if progress_callback is not None:
                progress_callback(index, total, status)
        return extracted

    def list_fonts(
        self,
        extensions: Optional[Iterable[str]] = None,
        *,
        refresh: bool = False,
        progress_callback: Optional[Callable[[float, str], None]] = None,
    ) -> list[FontEntry]:
        if refresh or not self._catalog:
            self._catalog = self.scan_font_catalog(
                extensions=extensions,
                progress_callback=progress_callback,
            )
        return list(self._catalog)

    def list_font_names(
        self,
        extensions: Optional[Iterable[str]] = None,
        *,
        refresh: bool = False,
    ) -> list[str]:
        seen: set[str] = set()
        names: list[str] = []
        for entry in self.list_fonts(extensions=extensions, refresh=refresh):
            key = entry.name.lower()
            if key in seen:
                continue
            seen.add(key)
            names.append(entry.name)
        return names

    def scan_font_catalog(
        self,
        extensions: Optional[Iterable[str]] = None,
        progress_callback: Optional[Callable[[float, str], None]] = None,
    ) -> list[FontEntry]:
        exts = _normalize_extensions(extensions or self.DEFAULT_SCAN_EXTENSIONS)
        entries: list[FontEntry] = []
        seen: set[tuple[str, str, int]] = set()
        self._emit_progress(progress_callback, 0.0, "Preparing font scan")

        # 1) Scan all directly present font files in fonts_root.
        all_files = sorted(self.fonts_root.rglob("*"))
        scan_candidates = [
            p
            for p in all_files
            if p.is_file()
            and not self._is_in_zip_cache(p)
            and p.suffix.lower() in exts
        ]
        total_scan_candidates = len(scan_candidates)
        for idx, p in enumerate(scan_candidates, start=1):
            if not p.is_file():
                continue
            if self._is_in_zip_cache(p):
                continue
            if p.suffix.lower() not in exts:
                continue
            entry = FontEntry(
                id=_build_id(p.stem, p.suffix),
                name=p.stem,
                path=p.resolve(),
                extension=p.suffix.lower().lstrip("."),
                source_kind="folder",
                source_container=p.parent.resolve(),
            )
            if _dedupe_key(entry) in seen:
                continue
            seen.add(_dedupe_key(entry))
            entries.append(entry)
            self._emit_progress(
                progress_callback,
                _phase_progress(idx, total_scan_candidates, 10.0, 55.0),
                f"Scanning folders {idx}/{total_scan_candidates}",
            )
        if total_scan_candidates == 0:
            self._emit_progress(progress_callback, 55.0, "No direct font files found")

        # 2) Scan zip archives (extract to cache if needed).
        if self.auto_extract:
            self._emit_progress(progress_callback, 56.0, "Processing zip archives")
            self.extract_all_archives_with_progress(
                progress_callback=lambda current, total, message: self._emit_progress(
                    progress_callback,
                    _phase_progress(current, total, 56.0, 78.0),
                    message,
                )
            )
        if self.zip_cache_root.exists():
            zip_files = [
                p
                for p in sorted(self.zip_cache_root.rglob("*"))
                if p.is_file() and p.name != ".stamp" and p.suffix.lower() in exts
            ]
            total_zip_files = len(zip_files)
            for idx, p in enumerate(zip_files, start=1):
                if not p.is_file():
                    continue
                if p.name == ".stamp":
                    continue
                if p.suffix.lower() not in exts:
                    continue
                source_zip = self._zip_source_for_cached_path(p)
                entry = FontEntry(
                    id=_build_id(p.stem, p.suffix),
                    name=p.stem,
                    path=p.resolve(),
                    extension=p.suffix.lower().lstrip("."),
                    source_kind="zip" if source_zip else "zip_cache",
                    source_container=source_zip if source_zip else p.parent.resolve(),
                )
                if _dedupe_key(entry) in seen:
                    continue
                seen.add(_dedupe_key(entry))
                entries.append(entry)
                self._emit_progress(
                    progress_callback,
                    _phase_progress(idx, total_zip_files, 79.0, 99.0),
                    f"Scanning extracted zips {idx}/{total_zip_files}",
                )
            if total_zip_files == 0:
                self._emit_progress(progress_callback, 99.0, "No extracted zip fonts found")

        entries.sort(key=lambda e: (e.name.lower(), e.extension, str(e.path).lower()))
        self._emit_progress(progress_callback, 100.0, f"Font scan complete: {len(entries)} entries")
        return entries

    def export_catalog(self, extensions: Optional[Iterable[str]] = None) -> list[dict[str, str]]:
        return [entry.as_dict() for entry in self.scan_font_catalog(extensions=extensions)]

    def save_catalog_json(
        self,
        out_path: Path | str,
        extensions: Optional[Iterable[str]] = None,
    ) -> Path:
        import json

        dest = Path(out_path)
        dest.parent.mkdir(parents=True, exist_ok=True)
        data = self.export_catalog(extensions=extensions)
        dest.write_text(json.dumps(data, indent=2), encoding="utf-8")
        return dest

    def get_font_path(
        self,
        name: str,
        *,
        extensions: Iterable[str] = RUNTIME_EXTENSIONS,
        strict: bool = False,
        refresh: bool = False,
    ) -> Path:
        fonts = self.list_fonts(extensions=extensions, refresh=refresh)
        if not fonts:
            raise FileNotFoundError(f"No font files found in {self.fonts_root}")
        normalized_target = _normalize(name)

        for entry in fonts:
            if _normalize(entry.name) == normalized_target:
                return entry.path

        partial = [e for e in fonts if normalized_target in _normalize(e.name)]
        if partial:
            for entry in partial:
                if "regular" in entry.name.lower():
                    return entry.path
            return partial[0].path

        if strict:
            raise FileNotFoundError(f"Font not found for query: {name}")
        return fonts[0].path

    def load_pygame_font(self, name: str, size: int):
        try:
            import pygame
        except ImportError as exc:
            raise RuntimeError("pygame is not installed") from exc
        font_path = self.get_font_path(name, extensions=self.RUNTIME_EXTENSIONS, strict=True)
        return pygame.font.Font(str(font_path), size)

    def load_tkinter_font(self, root, name: str, size: int = 24):
        """
        Creates a tkinter.font.Font using the requested catalog item.
        On Windows this tries private in-process registration first.
        """
        import tkinter.font as tkfont

        font_path = self.get_font_path(name, extensions=self.RUNTIME_EXTENSIONS, strict=True)
        if os.name == "nt":
            self.register_windows_font_by_path(font_path)
        candidate_families = _tk_family_candidates(font_path.stem)
        available_families = {f.lower(): f for f in tkfont.families(root)}
        selected = None
        for family in candidate_families:
            found = available_families.get(family.lower())
            if found:
                selected = found
                break
        if selected is None:
            selected = "TkDefaultFont"
        return tkfont.Font(root=root, family=selected, size=size)

    def register_windows_font(self, name: str) -> bool:
        """
        Registers a font privately for the current process on Windows.
        Returns True if registration succeeded.
        """
        if os.name != "nt":
            return False
        font_path = self.get_font_path(name, strict=True, extensions=self.RUNTIME_EXTENSIONS)
        return self.register_windows_font_by_path(font_path)

    def register_windows_font_by_path(self, font_path: Path | str) -> bool:
        if os.name != "nt":
            return False
        path = Path(font_path)
        FR_PRIVATE = 0x10
        added = ctypes.windll.gdi32.AddFontResourceExW(str(path), FR_PRIVATE, 0)
        if added > 0:
            _broadcast_font_change_windows()
        return added > 0

    def register_windows_all_runtime_fonts(self) -> int:
        if os.name != "nt":
            return 0
        count = 0
        for entry in self.list_fonts(extensions=self.RUNTIME_EXTENSIONS, refresh=True):
            if self.register_windows_font_by_path(entry.path):
                count += 1
        return count

    def unregister_windows_font(self, name: str) -> bool:
        if os.name != "nt":
            return False
        font_path = self.get_font_path(name, strict=True, extensions=self.RUNTIME_EXTENSIONS)
        return self.unregister_windows_font_by_path(font_path)

    def unregister_windows_font_by_path(self, font_path: Path | str) -> bool:
        if os.name != "nt":
            return False
        path = Path(font_path)
        FR_PRIVATE = 0x10
        removed = ctypes.windll.gdi32.RemoveFontResourceExW(str(path), FR_PRIVATE, 0)
        if removed > 0:
            _broadcast_font_change_windows()
        return removed > 0

    def _is_in_zip_cache(self, path: Path) -> bool:
        try:
            path.resolve().relative_to(self.zip_cache_root.resolve())
            return True
        except Exception:
            return False

    def _zip_source_for_cached_path(self, cached_file: Path) -> Optional[Path]:
        """
        Recover zip source from cache folder naming convention.
        """
        try:
            encoded_zip = cached_file.resolve().relative_to(self.zip_cache_root.resolve()).parts[0]
            possible = self.fonts_root / encoded_zip.replace("__", os.sep)
            if possible.suffix.lower() == ".zip" and possible.exists():
                return possible.resolve()
        except Exception:
            return None
        return None

    @staticmethod
    def _emit_progress(
        callback: Optional[Callable[[float, str], None]],
        percent: float,
        message: str,
    ) -> None:
        if callback is None:
            return
        try:
            callback(max(0.0, min(100.0, float(percent))), message)
        except Exception:
            return


def _normalize(value: str) -> str:
    return re.sub(r"[^a-z0-9]+", "", value.lower())


def _broadcast_font_change_windows() -> None:
    if os.name != "nt":
        return
    try:
        HWND_BROADCAST = 0xFFFF
        WM_FONTCHANGE = 0x001D
        SMTO_ABORTIFHUNG = 0x0002
        # Timeout-based broadcast prevents UI hangs from stuck windows.
        ctypes.windll.user32.SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_FONTCHANGE,
            0,
            0,
            SMTO_ABORTIFHUNG,
            80,
            0,
        )
    except Exception:
        return


def _phase_progress(current: int, total: int, start: float, end: float) -> float:
    if total <= 0:
        return end
    ratio = max(0.0, min(1.0, current / total))
    return start + ((end - start) * ratio)


def _safe_slug(value: str) -> str:
    # Keep reversible path-like hint and strip dangerous characters.
    cleaned = re.sub(r"[^a-zA-Z0-9._/\\-]+", "_", value)
    return cleaned.replace("/", "__").replace("\\", "__")


def _normalize_extensions(exts: Iterable[str]) -> set[str]:
    normalized: set[str] = set()
    for ext in exts:
        normalized.add(f".{str(ext).lower().lstrip('.')}")
    return normalized


def _build_id(name: str, suffix: str) -> str:
    return f"{_normalize(name)}{suffix.lower()}"


def _dedupe_key(entry: FontEntry) -> tuple[str, str, int]:
    size = 0
    try:
        size = entry.path.stat().st_size
    except OSError:
        pass
    return (_normalize(entry.name), entry.extension.lower(), size)


def _tk_family_candidates(stem: str) -> list[str]:
    base = stem.replace("_", " ").replace("-", " ")
    base = re.sub(r"\s+", " ", base).strip()
    variants = {
        base,
        re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", base),
        _strip_style_suffix(base),
        _strip_style_suffix(re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", base)),
    }
    out = [v.strip() for v in variants if v and v.strip()]
    out.sort(key=len, reverse=True)
    return out


def _strip_style_suffix(name: str) -> str:
    suffixes = (
        " Regular",
        " Bold",
        " Italic",
        " Light",
        " BoldItalic",
        " LightItalic",
    )
    out = name
    for suffix in suffixes:
        if out.endswith(suffix):
            out = out[: -len(suffix)].strip()
    return out


def create_default_loader() -> SegmentedFontLoader:
    return SegmentedFontLoader()


def load_font_catalog(
    fonts_root: Optional[Path | str] = None,
    extensions: Optional[Iterable[str]] = None,
) -> list[FontEntry]:
    loader = SegmentedFontLoader(fonts_root=fonts_root)
    return loader.list_fonts(extensions=extensions, refresh=True)


def load_font_names(
    fonts_root: Optional[Path | str] = None,
    extensions: Optional[Iterable[str]] = None,
) -> list[str]:
    loader = SegmentedFontLoader(fonts_root=fonts_root)
    return loader.list_font_names(extensions=extensions, refresh=True)
