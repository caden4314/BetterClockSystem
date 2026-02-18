from __future__ import annotations

import queue
import re
import threading
import tkinter as tk
from tkinter import ttk, font as tkfont

from font_loader import SegmentedFontLoader

try:
    from fontTools.ttLib import TTFont  # type: ignore
except Exception:  # pragma: no cover - optional dependency
    TTFont = None


class FontLoaderDemo:
    def __init__(self) -> None:
        self.root = tk.Tk()
        self.root.title("BetterClock Font Loader Demo (Tkinter)")
        self.root.geometry("1200x760")
        self.root.minsize(900, 600)
        self.root.configure(bg="#05131d")

        # Keep startup instant; extraction/scan is done in background worker.
        self.loader = SegmentedFontLoader(auto_extract=False)
        self.entries = []
        self.current_tk_font = None
        self._registered_windows: set[str] = set()
        self._family_lookup: dict[str, str] = {}
        self._unicode_cache: dict[str, list[int]] = {}
        self._unicode_job_token = 0
        self._unicode_pending_path: str | None = None
        self._scan_queue: queue.Queue[tuple[str, object]] = queue.Queue()
        self._unicode_queue: queue.Queue[tuple[int, str, list[int] | str]] = queue.Queue()
        self._apply_after_id: str | None = None

        self.preview_text_var = tk.StringVar(
            value="12:34:56  PM   BetterClock   2026-02-13"
        )
        self.size_var = tk.IntVar(value=64)
        self.status_var = tk.StringVar(value="Initializing...")
        self.scan_progress_var = tk.DoubleVar(value=0.0)
        self.scan_phase_var = tk.StringVar(value="Idle")
        self.unicode_info_var = tk.StringVar(
            value="Unicode preview loading... (install 'fonttools' for full cmap scan)"
        )
        self._is_scanning = False

        self._build_ui()
        self._refresh_family_lookup()
        self.start_catalog_scan(select_first=True)
        self.root.after(50, self._poll_queues)

    def _build_ui(self) -> None:
        container = ttk.Frame(self.root, padding=12)
        container.pack(fill=tk.BOTH, expand=True)

        self.paned = ttk.Panedwindow(container, orient=tk.HORIZONTAL)
        self.paned.pack(fill=tk.BOTH, expand=True)

        left = ttk.Frame(self.paned, padding=(0, 0, 10, 0))
        right = ttk.Frame(self.paned)
        self.paned.add(left, weight=1)
        self.paned.add(right, weight=3)
        self.root.after(50, lambda: self._set_initial_pane_ratio(0.30))

        left.columnconfigure(0, weight=1)
        left.rowconfigure(1, weight=1)

        ttk.Label(left, text="Discovered Fonts").grid(row=0, column=0, sticky="w")
        list_frame = ttk.Frame(left)
        list_frame.grid(row=1, column=0, sticky="nsew")
        list_frame.rowconfigure(0, weight=1)
        list_frame.columnconfigure(0, weight=1)

        self.font_list = tk.Listbox(
            list_frame,
            activestyle="dotbox",
            bg="#061826",
            fg="#a9f2ff",
            selectbackground="#0b4156",
            selectforeground="#ffffff",
            highlightthickness=0,
            font=("Consolas", 11),
        )
        font_list_y = ttk.Scrollbar(list_frame, orient=tk.VERTICAL, command=self.font_list.yview)
        font_list_x = ttk.Scrollbar(list_frame, orient=tk.HORIZONTAL, command=self.font_list.xview)
        self.font_list.configure(yscrollcommand=font_list_y.set, xscrollcommand=font_list_x.set)
        self.font_list.grid(row=0, column=0, sticky="nsew")
        font_list_y.grid(row=0, column=1, sticky="ns")
        font_list_x.grid(row=1, column=0, sticky="ew")
        self.font_list.bind("<<ListboxSelect>>", self.on_font_selected)

        btn_row = ttk.Frame(left)
        btn_row.grid(row=2, column=0, sticky="ew", pady=(8, 0))
        self.refresh_btn = ttk.Button(btn_row, text="Refresh", command=self.start_catalog_scan)
        self.refresh_btn.pack(side=tk.LEFT)
        self.apply_btn = ttk.Button(btn_row, text="Apply", command=self.apply_selected_font)
        self.apply_btn.pack(
            side=tk.LEFT, padx=(8, 0)
        )
        self.scan_progress = ttk.Progressbar(
            left,
            orient=tk.HORIZONTAL,
            mode="determinate",
            variable=self.scan_progress_var,
            maximum=100.0,
        )
        self.scan_progress.grid(row=3, column=0, sticky="ew", pady=(8, 0))
        ttk.Label(left, textvariable=self.scan_phase_var).grid(
            row=4, column=0, sticky="w", pady=(4, 0)
        )

        right.rowconfigure(3, weight=1)
        right.columnconfigure(0, weight=1)

        controls = ttk.Frame(right)
        controls.grid(row=0, column=0, sticky="ew")
        controls.columnconfigure(1, weight=1)

        ttk.Label(controls, text="Preview Text:").grid(row=0, column=0, sticky="w")
        ttk.Entry(controls, textvariable=self.preview_text_var).grid(
            row=0, column=1, sticky="ew", padx=(8, 0)
        )

        ttk.Label(controls, text="Size:").grid(row=1, column=0, sticky="w", pady=(8, 0))
        ttk.Scale(
            controls,
            from_=16,
            to=180,
            orient=tk.HORIZONTAL,
            variable=self.size_var,
            command=self._on_size_slider,
        ).grid(row=1, column=1, sticky="ew", padx=(8, 0), pady=(8, 0))

        self.preview = tk.Label(
            right,
            textvariable=self.preview_text_var,
            bg="#02101a",
            fg="#b9f8ff",
            anchor="center",
            justify="center",
            pady=24,
        )
        self.preview.grid(row=1, column=0, sticky="ew", pady=(12, 8))

        self.meta_text = tk.Text(
            right,
            height=7,
            wrap=tk.WORD,
            bg="#031521",
            fg="#9ed9e4",
            insertbackground="#9ed9e4",
            relief=tk.FLAT,
        )
        self.meta_text.grid(row=2, column=0, sticky="ew")
        self.meta_text.configure(state=tk.DISABLED)

        unicode_frame = ttk.Frame(right)
        unicode_frame.grid(row=3, column=0, sticky="nsew", pady=(8, 0))
        unicode_frame.rowconfigure(1, weight=1)
        unicode_frame.columnconfigure(0, weight=1)

        ttk.Label(unicode_frame, textvariable=self.unicode_info_var).grid(
            row=0, column=0, sticky="w"
        )
        self.unicode_text = tk.Text(
            unicode_frame,
            wrap=tk.NONE,
            bg="#020e16",
            fg="#b6f5ff",
            relief=tk.FLAT,
            height=10,
        )
        self.unicode_text.grid(row=1, column=0, sticky="nsew")
        self.unicode_text.configure(state=tk.DISABLED)

        self.status = ttk.Label(right, textvariable=self.status_var)
        self.status.grid(row=4, column=0, sticky="sw", pady=(8, 0))

    def start_catalog_scan(self, select_first: bool = False) -> None:
        if self._is_scanning:
            return
        self._set_scan_state(True)
        self.scan_progress_var.set(0.0)
        self.scan_phase_var.set("Starting scan...")
        self.status_var.set("Scanning font folders and zips...")

        def worker() -> None:
            try:
                entries = self.loader.list_fonts(
                    extensions=self.loader.RUNTIME_EXTENSIONS,
                    refresh=True,
                    progress_callback=lambda percent, message: self._scan_queue.put(
                        ("progress", (percent, message))
                    ),
                )
                self._scan_queue.put(("ok", (entries, select_first)))
            except Exception as exc:
                self._scan_queue.put(("err", exc))

        threading.Thread(target=worker, daemon=True).start()

    def refresh_catalog_ui(self, select_first: bool = False) -> None:
        # Must be writable/selectable while we repopulate.
        self.font_list.configure(state=tk.NORMAL)
        self.font_list.delete(0, tk.END)
        for entry in self.entries:
            self.font_list.insert(
                tk.END,
                f"{entry.name:<34} [{entry.extension}] ({entry.source_kind})",
            )

        self.status_var.set(
            f"Loaded {len(self.entries)} font entries from {self.loader.fonts_root}"
        )
        if self.entries and (select_first or self.font_list.curselection() == ()):
            self.font_list.selection_set(0)
            self.font_list.activate(0)
            self.font_list.see(0)
            self.apply_selected_font()
        elif not self.entries:
            self._set_unicode_text("(no fonts found)")
            self.unicode_info_var.set("No fonts discovered")

    def on_font_selected(self, _event=None) -> None:
        self.apply_selected_font()

    def _on_size_slider(self, _value: str) -> None:
        # Debounce slider updates to keep Tk responsive.
        if self._apply_after_id is not None:
            try:
                self.root.after_cancel(self._apply_after_id)
            except Exception:
                pass
        self._apply_after_id = self.root.after(70, self._apply_after_debounced)

    def _apply_after_debounced(self) -> None:
        self._apply_after_id = None
        self.apply_selected_font()

    def apply_selected_font(self) -> None:
        index = self._selected_index()
        if index is None:
            return
        entry = self.entries[index]
        size = max(8, int(self.size_var.get()))

        try:
            self.current_tk_font = self._create_tk_font(entry.path, entry.name, size)
            self.preview.configure(font=self.current_tk_font, fg="#b9f8ff")
            unicode_font_size = max(10, min(22, size // 2))
            self.unicode_text.configure(font=(self.current_tk_font.actual("family"), unicode_font_size))
            actual_family = self.current_tk_font.actual("family")
            self.status_var.set(f"Applied: {entry.name} (family: {actual_family})")
        except Exception as exc:
            self.preview.configure(font=("Consolas", size))
            self.preview.configure(fg="#ff8e8e")
            self.unicode_text.configure(font=("Consolas", 12))
            self.status_var.set(f"Could not apply '{entry.name}': {exc}")
        self._set_meta(entry)
        self._queue_unicode_preview(entry.path)

    def _selected_index(self) -> int | None:
        selected = self.font_list.curselection()
        if not selected:
            return None
        return int(selected[0])

    def _set_meta(self, entry) -> None:
        lines = [
            f"Name: {entry.name}",
            f"ID: {entry.id}",
            f"Ext: {entry.extension}",
            f"Source kind: {entry.source_kind}",
            f"Source container: {entry.source_container}",
            f"Path: {entry.path}",
        ]
        self.meta_text.configure(state=tk.NORMAL)
        self.meta_text.delete("1.0", tk.END)
        self.meta_text.insert("1.0", "\n".join(lines))
        self.meta_text.configure(state=tk.DISABLED)

    def _create_tk_font(self, font_path, font_name: str, size: int) -> tkfont.Font:
        if self.root.tk.call("tk", "windowingsystem") == "win32":
            p = str(font_path)
            if p not in self._registered_windows:
                _ = self.loader.register_windows_font_by_path(font_path)
                self._registered_windows.add(p)
            self._refresh_family_lookup()
        family = self._choose_family(font_name, str(font_path))
        return tkfont.Font(root=self.root, family=family, size=size)

    def _refresh_family_lookup(self) -> None:
        families = tkfont.families(self.root)
        self._family_lookup = {f.lower(): f for f in families}

    def _choose_family(self, font_name: str, font_path: str) -> str:
        candidates = self._family_candidates(font_name, font_path)
        for raw in candidates:
            c = re.sub(r"\s+", " ", raw).strip()
            if not c:
                continue
            if c.lower() in self._family_lookup:
                return self._family_lookup[c.lower()]
        # Try first candidate directly; Tk family list can be stale right after registration.
        if candidates:
            first = re.sub(r"\s+", " ", candidates[0]).strip()
            if first:
                return first
        if "consolas" in self._family_lookup:
            return self._family_lookup["consolas"]
        return "TkFixedFont"

    def _family_candidates(self, font_name: str, font_path: str) -> list[str]:
        out: list[str] = []
        out.extend(self._name_variants(font_name))
        out.extend(self._fonttools_family_candidates(font_path))
        dedup: list[str] = []
        seen: set[str] = set()
        for item in out:
            cleaned = re.sub(r"\s+", " ", item).strip()
            if not cleaned:
                continue
            key = cleaned.lower()
            if key in seen:
                continue
            seen.add(key)
            dedup.append(cleaned)
        return dedup

    def _name_variants(self, raw_name: str) -> list[str]:
        values = [raw_name, raw_name.replace("-", " ")]
        split_camel = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", raw_name)
        values.append(split_camel)
        values.append(split_camel.replace("-", " "))
        more = []
        for v in values:
            more.extend([v, self._strip_style(v)])
        return [v for v in more if v]

    def _fonttools_family_candidates(self, font_path: str) -> list[str]:
        if TTFont is None:
            return []
        try:
            with TTFont(font_path, lazy=True, fontNumber=0) as font:
                name_table = font.get("name")
                if name_table is None:
                    return []
                candidates: list[str] = []
                for name_id in (1, 2, 4, 16, 17):
                    for rec in name_table.names:
                        if rec.nameID != name_id:
                            continue
                        try:
                            text = rec.toUnicode()
                        except Exception:
                            continue
                        if text:
                            candidates.append(text)
                            candidates.append(self._strip_style(text))
                return candidates
        except Exception:
            return []

    def _strip_style(self, value: str) -> str:
        suffixes = (
            " Regular",
            " Bold",
            " Italic",
            " Light",
            " Bold Italic",
            " Light Italic",
            " BoldItalic",
            " LightItalic",
        )
        out = value
        for suffix in suffixes:
            if out.endswith(suffix):
                out = out[: -len(suffix)].strip()
        return out

    def _queue_unicode_preview(self, font_path) -> None:
        path = str(font_path)
        if path in self._unicode_cache:
            self._unicode_job_token += 1
            token = self._unicode_job_token
            self._unicode_queue.put((token, path, self._unicode_cache[path]))
            return
        if self._unicode_pending_path == path:
            return

        self._unicode_job_token += 1
        token = self._unicode_job_token
        self._unicode_pending_path = path
        self.unicode_info_var.set("Loading Unicode glyph map...")
        self._set_unicode_text("Scanning glyphs...")

        def worker() -> None:
            try:
                points = self._extract_unicode_points(path)
                self._unicode_queue.put((token, path, points))
            except Exception as exc:
                self._unicode_queue.put((token, path, f"Unicode scan failed: {exc}"))

        threading.Thread(target=worker, daemon=True).start()

    def _extract_unicode_points(self, font_path: str) -> list[int]:
        if TTFont is None:
            # Fallback sample set if fonttools is missing.
            sample = list(range(0x20, 0x7F))
            sample.extend(range(0xA0, 0x100))
            return sample

        points: set[int] = set()
        with TTFont(font_path, lazy=True, fontNumber=0) as font:
            cmap = font.getBestCmap() or {}
            for cp in cmap.keys():
                if isinstance(cp, int):
                    points.add(cp)
        sorted_points = sorted(points)
        if not sorted_points:
            return list(range(0x20, 0x7F))
        return sorted_points

    def _render_unicode_points(self, points: list[int]) -> None:
        printable = []
        for cp in points:
            if cp < 0x20:
                continue
            if 0xD800 <= cp <= 0xDFFF:
                continue
            if cp > 0x10FFFF:
                continue
            try:
                ch = chr(cp)
            except ValueError:
                continue
            if ch.isspace() and cp != 0x20:
                continue
            printable.append(cp)

        preview_max = 384
        preview = printable[:preview_max]
        rows = []
        row_width = 24
        for i in range(0, len(preview), row_width):
            chunk = preview[i : i + row_width]
            rows.append("".join(chr(cp) for cp in chunk))

        if not rows:
            rows = ["(no printable glyphs found)"]

        self._set_unicode_text("\n".join(rows))
        extra = ""
        if TTFont is None:
            extra = " | install 'fonttools' for real cmap"
        self.unicode_info_var.set(
            f"Unicode glyphs: {len(printable)} | showing first {len(preview)}{extra}"
        )

    def _set_unicode_text(self, value: str) -> None:
        self.unicode_text.configure(state=tk.NORMAL)
        self.unicode_text.delete("1.0", tk.END)
        self.unicode_text.insert("1.0", value)
        self.unicode_text.configure(state=tk.DISABLED)

    def _poll_queues(self) -> None:
        for _ in range(120):
            try:
                kind, payload = self._scan_queue.get_nowait()
            except queue.Empty:
                break
            if kind == "progress":
                percent, message = payload  # type: ignore[misc]
                self.scan_progress_var.set(float(percent))
                self.scan_phase_var.set(str(message))
            elif kind == "ok":
                self.entries, select_first = payload  # type: ignore[assignment]
                # Re-enable controls first so listbox selection/preview can initialize.
                self._set_scan_state(False)
                self.refresh_catalog_ui(select_first=bool(select_first))
                self.scan_progress_var.set(100.0)
                self.scan_phase_var.set("Scan complete")
            else:
                self.status_var.set(f"Font scan failed: {payload}")
                self.scan_phase_var.set("Scan failed")
                self._set_scan_state(False)

        for _ in range(40):
            try:
                token, path, payload = self._unicode_queue.get_nowait()
            except queue.Empty:
                break
            if token != self._unicode_job_token:
                continue
            self._unicode_pending_path = None
            if isinstance(payload, str):
                self.unicode_info_var.set(payload)
                self._set_unicode_text("(unicode preview unavailable)")
                continue
            self._unicode_cache[path] = payload
            self._render_unicode_points(payload)

        self.root.after(50, self._poll_queues)

    def run(self) -> None:
        self.root.mainloop()

    def _set_initial_pane_ratio(self, ratio: float) -> None:
        try:
            total = self.paned.winfo_width()
            if total <= 0:
                return
            desired = int(total * ratio)
            desired = max(240, min(desired, total - 300))
            self.paned.sashpos(0, desired)
        except Exception:
            return

    def _set_scan_state(self, scanning: bool) -> None:
        self._is_scanning = scanning
        if scanning:
            self.refresh_btn.configure(state=tk.DISABLED)
            self.apply_btn.configure(state=tk.DISABLED)
            self.font_list.configure(state=tk.DISABLED)
        else:
            self.refresh_btn.configure(state=tk.NORMAL)
            self.apply_btn.configure(state=tk.NORMAL)
            self.font_list.configure(state=tk.NORMAL)


if __name__ == "__main__":
    FontLoaderDemo().run()
