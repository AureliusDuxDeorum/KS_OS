#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Builds docs/design/KS_OS-Icon-List.pdf from docs/design/icon-list.txt.

Usage: python3 tools/make_icon_pdf.py [source.txt] [output.pdf]
Needs: reportlab (pip install reportlab) and the DejaVu fonts. Deterministic: the same input gives the same PDF.
Source format: see docs/design/README.md.
"""
import re
import sys
from pathlib import Path

from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle
from reportlab.lib.units import mm
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.platypus import (BaseDocTemplate, Frame, KeepTogether, PageBreak, PageTemplate,
                                Paragraph, Spacer, Table, TableStyle)

ROOT = Path(__file__).resolve().parent.parent
SRC = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "docs/design/icon-list.txt"
OUT = Path(sys.argv[2]) if len(sys.argv) > 2 else ROOT / "docs/design/KS_OS-Icon-List.pdf"
FONT_DIR = Path("/usr/share/fonts/truetype/dejavu")
for name, f in [("Sans", "DejaVuSans.ttf"), ("Sans-Bold", "DejaVuSans-Bold.ttf"), ("Mono", "DejaVuSansMono.ttf")]:
    pdfmetrics.registerFont(TTFont(name, str(FONT_DIR / f)))

KINDS = {
    "brand": "brand mark", "UI": "UI glyph", "status": "status glyph", "file": "file / folder icon",
    "device": "device glyph", "network": "network glyph", "security": "security glyph",
    "platform": "platform-trust glyph", "settings": "settings category", "power": "power / session",
    "app": "application icon", "cursor": "mouse cursor", "dev": "developer glyph",
    "access": "accessibility glyph", "misc": "miscellaneous glyph", "badge": "badge overlay",
}
PRIOS = {
    "P0": "needed for the first real UI",
    "P1": "full settings and core apps",
    "P2": "developer / admin / enterprise tools",
    "P3": "long tail",
}

# ---------------------------------------------------------------- parse & validate
sections = []   # (title, [entries])
entry_re = re.compile(r"^(?P<name>[^>]+?)\s*->\s*(?P<vars>.+?)\s*\[(?P<tag>[^\]]+)\]\s*$")
seen = set()
errors = []
for n, raw in enumerate(SRC.read_text(encoding="utf-8").splitlines(), 1):
    line = raw.strip()
    if not line:
        continue
    if line.startswith("#"):
        sections.append((re.sub(r"^\d+\.\s*", "", line.lstrip("# ").strip()), []))
        continue
    m = entry_re.match(line)
    if not m or not sections:
        errors.append(f"line {n}: cannot parse: {line[:70]}")
        continue
    name = m["name"].strip()
    variants = [v.strip() for v in m["vars"].split("|") if v.strip()]
    prio, _, kind = [p.strip() for p in m["tag"].partition(",")]
    if name.lower() in seen:
        errors.append(f"line {n}: duplicate icon type '{name}'")
    seen.add(name.lower())
    if not variants:
        errors.append(f"line {n}: '{name}' has no variants")
    if prio not in PRIOS:
        errors.append(f"line {n}: '{name}' has priority '{prio}', expected one of {sorted(PRIOS)}")
    if kind not in KINDS:
        errors.append(f"line {n}: '{name}' has kind '{kind}', expected one of {sorted(KINDS)}")
    sections[-1][1].append((name, variants, prio, kind))
if errors:
    sys.exit("icon-list.txt is invalid:\n  " + "\n  ".join(errors))

total_types = sum(len(e) for _, e in sections)
total_variants = sum(len(v) for _, e in sections for _, v, _, _ in e)
by_prio = {p: sum(1 for _, e in sections for x in e if x[2] == p) for p in PRIOS}
var_by_prio = {p: sum(len(x[1]) for _, e in sections for x in e if x[2] == p) for p in PRIOS}


def letters(i):
    s = ""
    i += 1
    while i:
        i, r = divmod(i - 1, 26)
        s = chr(97 + r) + s
    return s


# ---------------------------------------------------------------- styles
INK = "#000000"
GRAY = "#555555"
st = lambda **k: ParagraphStyle("x", **{"fontName": "Sans", "textColor": INK, **k})
H_TITLE = st(fontName="Sans-Bold", fontSize=26, leading=30, spaceAfter=4)
H_SUB = st(fontSize=11, leading=15, textColor=GRAY, spaceAfter=10)
H1 = st(fontName="Sans-Bold", fontSize=14, leading=18, spaceBefore=10, spaceAfter=4)
BODY = st(fontSize=9, leading=13, spaceAfter=3)
SMALL = st(fontSize=8, leading=11, textColor=GRAY)
SEC = st(fontName="Sans-Bold", fontSize=12, leading=15, spaceBefore=10, spaceAfter=5)
ENTRY = st(fontName="Sans-Bold", fontSize=10, leading=13)
TAG = st(fontName="Mono", fontSize=7.5, leading=10, textColor=GRAY, alignment=2)
VARS = st(fontSize=8.5, leading=12.5, leftIndent=16)

SERIAL = {"n": 0}


class Doc(BaseDocTemplate):
    cur = ""
    page_start = ""

    def afterFlowable(self, f):
        t = getattr(f, "_section_title", None)
        if t:
            self.cur = t


def on_page(c, doc):
    doc.page_start = doc.cur
    c.saveState()
    c.setFont("Sans", 7.5)
    c.setFillColor(GRAY)
    c.drawString(18 * mm, 10 * mm, "KS_OS icon list")
    c.drawRightString(A4[0] - 18 * mm, 10 * mm, f"page {doc.page}")
    c.setStrokeColor(GRAY)
    c.setLineWidth(0.4)
    c.line(18 * mm, 14 * mm, A4[0] - 18 * mm, 14 * mm)
    c.restoreState()


def on_page_end(c, doc):
    if doc.page > 2:
        c.saveState()
        c.setFont("Sans", 8)
        c.setFillColor(GRAY)
        c.drawString(18 * mm, A4[1] - 12 * mm, doc.cur or doc.page_start)
        c.drawRightString(A4[0] - 18 * mm, A4[1] - 12 * mm, "KS_OS icon list")
        c.line(18 * mm, A4[1] - 14 * mm, A4[0] - 18 * mm, A4[1] - 14 * mm)
        c.restoreState()


doc = Doc(str(OUT), pagesize=A4, leftMargin=18 * mm, rightMargin=18 * mm, topMargin=20 * mm,
          bottomMargin=20 * mm, title="KS_OS icon list", author="KS_OS", subject="Every icon type to design",
          invariant=1)
doc.addPageTemplates([PageTemplate(id="p", frames=[Frame(18 * mm, 18 * mm, A4[0] - 36 * mm, A4[1] - 40 * mm,
                                                         leftPadding=0, rightPadding=0, topPadding=0,
                                                         bottomPadding=0)],
                                   onPage=on_page, onPageEnd=on_page_end)])
W = A4[0] - 36 * mm

story = []

# ---- cover
version = "unversioned"
cargo = (ROOT / "Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', cargo, re.M)
if m:
    version = m.group(1)
story += [
    Paragraph("KS_OS", H_TITLE),
    Paragraph(f"Icon list — every icon type to design (OS version {version})", H_SUB),
    Spacer(1, 6),
    Paragraph(
        f"This document lists <b>{total_types} icon types</b> with <b>{total_variants} variants</b> in "
        f"{len(sections)} groups. Every variant has a number such as <b>27d</b>: icon type 27, variant d. "
        "Use these numbers when you report progress (\"27d is done\", \"redo 33b\"). The same list, in plain text, "
        "lives in the repository as <font name='Mono'>docs/design/icon-list.txt</font>; this PDF is generated from it.",
        BODY),
    Paragraph("How to read an entry", H1),
    Paragraph("<b>27. Folder</b> &nbsp;&nbsp;<font name='Mono' size='8'>P0 · file / folder icon</font><br/>"
              "a empty &nbsp;·&nbsp; b empty! &nbsp;·&nbsp; c full &nbsp;·&nbsp; d open (animation) …", BODY),
    Paragraph("Each variant is one separate drawing (or one animation, delivered as a strip of frames). "
              "The empty box ☐ in front of a variant is for ticking it off.", BODY),
    Paragraph("Legend", H1),
]
leg = [["Tag", "Meaning", "Types", "Variants"]]
for p, d in PRIOS.items():
    leg.append([p, d, str(by_prio[p]), str(var_by_prio[p])])
leg.append(["", "total", str(total_types), str(total_variants)])
t = Table(leg, colWidths=[16 * mm, 100 * mm, 22 * mm, 22 * mm])
t.setStyle(TableStyle([("FONT", (0, 0), (-1, -1), "Sans", 8.5), ("FONT", (0, 0), (-1, 0), "Sans-Bold", 8.5),
                       ("FONT", (0, -1), (-1, -1), "Sans-Bold", 8.5), ("LINEBELOW", (0, 0), (-1, 0), 0.6, INK),
                       ("LINEABOVE", (0, -1), (-1, -1), 0.6, INK), ("ALIGN", (2, 0), (-1, -1), "RIGHT"),
                       ("TOPPADDING", (0, 0), (-1, -1), 2), ("BOTTOMPADDING", (0, 0), (-1, -1), 2)]))
story += [t, Spacer(1, 8)]
story.append(Paragraph(
    "<b>(animation)</b> means a sequence of frames (8–12, spinner 12, progress ring 16). "
    "<b>(alert)</b>, <b>!</b> and similar mark a state that needs attention. Because the design language is "
    "black / white / gray only, <b>no meaning may depend on colour</b>: success, warning, error and information must "
    "differ by <i>silhouette</i>. States such as hover, pressed, disabled and focus are produced by the OS and are "
    "not separate drawings; only <i>semantic</i> variants are listed. "
    "A <b>badge</b> is an overlay drawn once and composed onto folders, files, drives and apps (see group 19).", BODY))
kinds_rows = [["Kind tag", "What it is"]] + [[k, v] for k, v in KINDS.items()]
t2 = Table(kinds_rows, colWidths=[24 * mm, 80 * mm])
t2.setStyle(TableStyle([("FONT", (0, 0), (-1, -1), "Mono", 7.5), ("FONT", (0, 0), (-1, 0), "Sans-Bold", 8),
                        ("LINEBELOW", (0, 0), (-1, 0), 0.6, INK), ("TOPPADDING", (0, 0), (-1, -1), 1),
                        ("BOTTOMPADDING", (0, 0), (-1, -1), 1)]))
story += [Spacer(1, 4), t2, PageBreak()]

# ---- cheat sheet
rules = [
    ("Ink and meaning", "One ink colour on transparent (the OS recolours it: white on black, black on white, gray when disabled). "
     "Optionally one mid-gray second tone. Never let colour carry meaning — use silhouette."),
    ("Formats", "SVG, filled outlines only: no strokes, text, gradients, filters, masks, clip-paths, external references or scripts. "
     "One path per tone where possible. Coordinates on whole or half pixels."),
    ("UI glyph sizes", "24 × 24 master and a hand-snapped 16 × 16 (the kernel framebuffer has no hinting: at 16 px every stroke must sit on whole "
     "pixels). 20 × 20 is derived unless it looks wrong."),
    ("Metrics (24 px)", "2 px padding, 2 px nominal stroke (1.5 px for dense glyphs), one corner rule for all icons (radius 2 px or a chamfer — pick one), "
     "keyline shapes: circle 20, square 18, portrait 16 × 20, landscape 20 × 16."),
    ("App, folder and file icons", "Sizes 32, 48, 64, 128, 256 (master 256). Ink plus up to two gray tones. File types share one page base; "
     "draw the base once and only the glyph per type."),
    ("Badges", "Drawn once at 8 × 8 (16 px) and 12 × 12 (24 px), bottom-right overlay."),
    ("Animations", "Delivered as separate frames (no SMIL): spinner 12, progress ring 16, others 8–12; the first frame is also the static fallback."),
    ("Cursors", "32 × 32 master; record the hot-spot in the file name, e.g. ks-cursor-arrow@2-2.svg."),
    ("RTL", "Mark directional icons (back, forward, send, list indent …) in your notes; the OS mirrors them for right-to-left languages."),
    ("Naming", "ks-&lt;group&gt;-&lt;name&gt;-&lt;variant&gt;, lowercase with hyphens, e.g. ks-file-folder-open, ks-net-wifi-3-locked, "
     "ks-sec-lock-closed. The 16 px version is the same name plus @16."),
    ("Delivery folders", "assets/icons/ui · apps · files · badges · cursors, one SVG per variant."),
    ("Rights", "Icons you draw fall under the repository licence (Apache-2.0) unless you decide otherwise before committing; the KS name and "
     "logos stay governed by TRADEMARKS.md."),
]
story.append(Paragraph("The rules on one page", H1))
rt = Table([[Paragraph(f"<b>{a}</b>", BODY), Paragraph(b, BODY)] for a, b in rules], colWidths=[36 * mm, W - 36 * mm])
rt.setStyle(TableStyle([("VALIGN", (0, 0), (-1, -1), "TOP"), ("LINEBELOW", (0, 0), (-1, -1), 0.3, GRAY),
                        ("TOPPADDING", (0, 0), (-1, -1), 4), ("BOTTOMPADDING", (0, 0), (-1, -1), 4)]))
story += [rt, PageBreak()]

# ---- the list
num = 0
for si, (title, entries) in enumerate(sections, 1):
    p = Paragraph(f"{si}. {title}", SEC)
    p._section_title = f"{si}. {title}"
    first = True
    for name, variants, prio, kind in entries:
        num += 1
        head = Table([[Paragraph(f"{num}. {name}", ENTRY), Paragraph(f"{prio} · {KINDS[kind]}", TAG)]],
                     colWidths=[W - 55 * mm, 55 * mm])
        head.setStyle(TableStyle([("VALIGN", (0, 0), (-1, -1), "BOTTOM"), ("LEFTPADDING", (0, 0), (-1, -1), 0),
                                  ("RIGHTPADDING", (0, 0), (-1, -1), 0), ("TOPPADDING", (0, 0), (-1, -1), 5),
                                  ("BOTTOMPADDING", (0, 0), (-1, -1), 1), ("LINEBELOW", (0, 0), (-1, 0), 0.25, GRAY)]))
        vt = " &nbsp;·&nbsp; ".join(
            f"☐&nbsp;<font name='Mono' size='7'>{letters(i)}</font>&nbsp;{v.replace('&', '&amp;').replace('<', '&lt;')}"
            for i, v in enumerate(variants))
        block = [head, Paragraph(vt, VARS)]
        if first:
            block.insert(0, p)
            first = False
        story.append(KeepTogether(block))

# ---- closing page
story += [PageBreak(), Paragraph("Suggested order of work", H1)]
for s in [
    "<b>1. Pilot set (12 icons).</b> Settle the grid and the corner rule on: Folder (1 variant), Generic file, Settings gear, User, Lock, Wi-Fi 3 bars, "
    "Bluetooth, Battery full, Power off, Search, Close, Check mark. Show them at 16 and 24 px on black and on white before drawing more.",
    "<b>2. All P0 types</b> (see the cover for the count): the window controls, status glyphs, power and session icons, the network and device status "
    "icons, folders and the generic file, the cursors and the badges.",
    "<b>3. Badges and cursors</b> next, because every folder, file and app icon is composed with them.",
    "<b>4. P1 types</b>: all settings categories, the core apps, the remaining file types.",
    "<b>5. App icons last</b> (P1/P2): they use the finished glyph language at 256 px and are the most work per icon.",
    "<b>6. P2/P3</b> as the OS grows; this list is the source of truth and can be extended at any time "
    "(edit <font name='Mono'>docs/design/icon-list.txt</font>, then run <font name='Mono'>python3 tools/make_icon_pdf.py</font>).",
]:
    story.append(Paragraph(s, BODY))
story += [Spacer(1, 8), Paragraph("Notes for icons specific to KS_OS", H1),
          Paragraph("Group 10 contains icons that most operating systems do not have but KS_OS needs because of its design: "
                    "Secure Boot, verified boot chain, TPM (sealed / unsealing / failed), DMA protection (IOMMU on / off / blocked), "
                    "capability tokens (granted / revoked / delegated) and the privacy-in-use indicators. They are the visual identity of the "
                    "\"secure by construction\" idea, so they deserve the most care at 16 px, where they appear in the status area.", BODY)]
doc.build(story)
print(f"wrote {OUT}: {total_types} icon types, {total_variants} variants, {len(sections)} groups")
