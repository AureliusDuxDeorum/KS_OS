# Notes on German and EU law for KS_OS

**General information for the project owner, not legal advice.** I (the AI that wrote most of this
repository) am not a lawyer and may be wrong or out of date. Have a *Rechtsanwalt* with IT/IP experience confirm
the points that matter before relying on them, especially before KS_OS is distributed beyond a small
private circle or sold. The date of these notes is 2026-11-02.

## 1. What "private" does and does not change
- The licence only matters once the software is **given to someone else** — a friend, a tester, a public
  repository, a release download. Running your own build on your own machines needs no licence at all.
- Handing the boot image to anyone counts as distribution. That is when the Apache-2.0 terms (and the
  third-party notices that must travel with the image) apply.
- A **private** GitHub repository is not public distribution, but inviting collaborators or giving others
  access to the image already is.

## 2. Copyright (Urheberrecht) in the code
- German copyright (UrhG) arises automatically, without registration, for a *personal intellectual creation*
  (§ 2 Abs. 2 UrhG; software: §§ 69a ff. UrhG). The author is the natural person who created the work (§ 7 UrhG);
  **copyright itself cannot be transferred** (§ 29 UrhG), only licensed (*Nutzungsrechte*).
- **Employment:** if you wrote or directed the project as part of a job, § 69b UrhG gives the employer the
  economic rights to software created in the course of employment. Check your contract if that could apply.
- **AI-written code:** most of this code was produced by an AI under your direction. Under the prevailing view in
  Germany/EU, protection requires a *human* creative contribution; purely machine-generated output may have
  **no copyright holder**, while your own design decisions, selection, structure and edits can be protected.
  The practical consequences: (a) the licence still states your intent and is harmless for any unprotected
  parts; (b) you may not be able to enforce rights over parts that are not protected; (c) keep records of your
  direction (the ADRs and the decisions in this repository help). The law here is unsettled — ask a lawyer.
- `NOTICE` currently names "The KS_OS Authors". **Replace this with your legal name (or company)** when you
  decide how you want to be credited.

## 3. The Apache-2.0 licence under German law
- Free software licences are generally enforceable in Germany (German courts have enforced the GPL since the
  early 2000s; the same reasoning is applied to other open-source licences).
- **Liability disclaimers have limits in Germany:** liability for intent and gross negligence, for injury to life,
  body or health, and under the Product Liability Act cannot be excluded (§§ 276 Abs. 3, 309 BGB, ProdHaftG).
  Software provided free of charge is usually treated like a gift (§ 521 BGB, liability limited to intent and
  gross negligence). The "AS IS" wording in Apache-2.0 is therefore effective *to the extent German law allows*;
  it is not a shield against everything. Do not promise fitness for any purpose in your documentation.
- **Selling** KS_OS (or offering it as a product) changes this: statutory warranty (*Gewährleistung*, §§ 434 ff.
  BGB, including digital-product rules §§ 327 ff. BGB) can then apply and cannot simply be disclaimed.

## 4. Third-party code and firmware
- Every dependency is MIT, Apache-2.0 or BSD-style (checked automatically by `cargo deny` in CI). Their licences
  require that **copyright notices and licence texts accompany binary distributions**. `cargo xtask release`
  generates `THIRD-PARTY-LICENSES.txt` for this and ships it with the image — keep doing that.
- The Limine bootloader (BSD-2-Clause) is redistributed inside the image with its notice; its checksum is pinned.
- **Firmware blobs (MediaTek/Realtek Bluetooth) are never shipped**: their redistribution terms are separate
  and the design deliberately has each user copy them from their own system. Keep it that way unless you obtain
  explicit redistribution rights.
- If you ever copy code from Linux or other GPL projects, the GPL obligations apply to that code; keep such
  code in clearly separate components (this is also what the microkernel design is for).

## 5. Names, logos and branding (Markenrecht)
- A trademark in Germany arises by **registration** (DPMA; for the EU, EUIPO), by **use with market recognition**
  (*Verkehrsgeltung*, § 4 Nr. 2 MarkenG — hard to prove for a new project), or as a *Werktitel* in some cases.
  Simply using "KS" does not secure it.
- Before investing in the "KS" branding: run a **conflict search** (existing marks for software, Nice classes 9
  and 42, are the relevant ones) — two-letter names are crowded.
- Apache-2.0 does not license your marks (see `TRADEMARKS.md`). Logos are also copyright works and fonts have
  their own licences — keep branding assets and their licences in a separate folder (Phase 8).
- Naming other products in the docs (Ubuntu, Arch, Logitech, …) is fine when only describing compatibility;
  do not use their logos.

## 6. Personal data (GDPR / DSGVO, TDDDG)
- KS_OS currently collects **no personal data and sends nothing over a network**. Keep that true by default.
- Stored locally: Bluetooth bonds (device addresses and keys) in firmware NV storage. Device identifiers can be
  personal data, but keeping them on the user's own machine is not a transfer.
- When networking arrives (Phase 7): any telemetry, update check or crash reporting needs a legal basis
  (consent or legitimate interest, transparency, data minimisation) and must be opt-in by default.

## 7. EU rules likely to matter if KS_OS is ever offered commercially
- **Cyber Resilience Act (Regulation (EU) 2024/2847):** applies to "products with digital elements" made available
  *in the course of a commercial activity*; purely non-commercial open source is largely outside it, but selling
  support, bundling it in a commercial product or running it as a business can bring it in. Reporting duties for
  actively exploited vulnerabilities start in 2026 and the main obligations in December 2027. `SECURITY.md`
  (a vulnerability-reporting contact) is the first step.
- **Product liability:** the revised EU Product Liability Directive (2024/2853) treats software as a product;
  member-state implementation is due by December 2026.
- **Cryptography/export:** the Bluetooth security code uses standard, mass-market cryptography; open-source
  publication is generally unproblematic, but dual-use export rules (EU 2021/821, US EAR) exist — check when
  distributing commercially or outside the EU/US.
- **Radio equipment:** the Bluetooth radio's regulatory approval belongs to the hardware makers; KS_OS does
  not change radio parameters beyond what the controller firmware allows.
- **Impressum / provider identification** (§ 5 DDG): applies to *geschäftsmäßige* public online offerings; a public
  project website or store listing may need one; a private repository normally does not.

## 8. Practical checklist
- [x] Licence chosen (Apache-2.0), `LICENSE`, `NOTICE`, SPDX headers, third-party notices generated on release.
- [x] Contributions under the same licence via DCO sign-off (`CONTRIBUTING.md`).
- [x] `SECURITY.md` and `TRADEMARKS.md` in place.
- [ ] Owner: put your legal name/entity in `NOTICE`; add a security contact in `SECURITY.md`.
- [ ] Owner: decide on registering the "KS" name/logo (conflict search first).
- [ ] Owner: have a lawyer review sections 2 (AI-written code) and 3 (liability) if KS_OS leaves your control.
