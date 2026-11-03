# Design assets

- `icon-list.txt` — every icon type to design, with its variants (source of truth, plain text).
- `KS_OS-Icon-List.pdf` — the same list as a numbered PDF (cover, one-page rules, the list, work order). Regenerate after editing the text file:

```bash
python3 tools/make_icon_pdf.py        # needs: pip install reportlab, and the DejaVu fonts
```

Source format, one icon type per line:

```
# 5. Folders                                          <- starts a group
Folder -> empty | empty! | open (animation) | locked   [P0, file]
```

Left of `->` the icon type, `|` separates variants, `[priority, kind]` at the end. Priorities: P0 first real UI, P1 settings and core apps,
P2 developer/admin/enterprise tools, P3 long tail. Kinds: brand, UI, status, file, device, network, security, platform, settings, power, app, cursor, dev,
access, misc, badge. The generator numbers types (27) and letters variants (27d) and refuses duplicate names or unknown tags.
