# Security policy

KS_OS is pre-release software (version 0.x: not finished) and has not had an independent security review.
Do not use it to protect anything valuable yet.

## Reporting a vulnerability
Please report suspected vulnerabilities **privately** to the project owner (Nils Seidenberg, <contactns@kanasaka.com>) rather than opening a public issue. Include the version
(`KS_OS-vX.Y.Z`), what you did, what happened, and a photo or copy of the on-screen kernel log (`L` key)
if you have one. You will get an acknowledgement; fixes ship as a bug-patch release (`Z`) and are noted in
`CHANGELOG.md`.

## Scope and known gaps
The known, deliberately open security items are tracked per stage in `docs/hardening/`
(for example: Bluetooth bonds are stored unencrypted; automatic pairing needs an authorisation policy;
IOMMU protection is not enabled yet).
