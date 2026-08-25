# Security policy

Torben App is pre-release software. Security reports are accepted for the current `main` branch.

Please report suspected vulnerabilities privately through GitHub Security Advisories for `TorbenXiong/torben-app`. Include affected versions, reproduction steps, impact, and any suggested remediation. Do not include credentials or unrelated personal data.

Official installation flows fail closed when transport, hash, signature, archive safety, or health checks fail. Reports that identify a bypass of those checks are especially valuable.

Official Torben App updates must use the fixed HTTPS GitHub Release endpoint and a Base64-encoded
minisign public key compiled into the application. Development builds without that key do not check for updates.
The updater must verify the downloaded artifact before installation; reports of endpoint override,
key substitution, signature bypass, or silent installation are security issues.
