#!/usr/bin/env python3
"""Validate local links in the dependency-free CLI documentation site."""

from __future__ import annotations

from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"


class LinkParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.links: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr = "href" if tag in {"a", "link"} else "src" if tag == "script" else None
        if attr is None:
            return
        values = dict(attrs)
        if values.get(attr):
            self.links.append(values[attr] or "")


def target_for(page: Path, link: str) -> Path | None:
    parsed = urlsplit(link)
    if parsed.scheme or parsed.netloc or link.startswith("#"):
        return None
    raw_path = parsed.path
    if not raw_path:
        return None
    target = (page.parent / raw_path).resolve()
    if raw_path.endswith("/"):
        target /= "index.html"
    return target


def main() -> None:
    failures: list[str] = []
    pages = sorted(DOCS.rglob("*.html"))
    if not pages:
        raise SystemExit("no documentation pages found")
    for page in pages:
        parser = LinkParser()
        parser.feed(page.read_text(encoding="utf-8"))
        for link in parser.links:
            target = target_for(page, link)
            if target is not None and not target.exists():
                failures.append(f"{page.relative_to(ROOT)}: missing {link}")
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"validated {len(pages)} documentation pages")


if __name__ == "__main__":
    main()
