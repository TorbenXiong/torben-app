# Node.js signature fixtures

`v24.19.0-SHASUMS256.txt` and the detached signature encoded in the Core tests come from:

`https://nodejs.org/dist/v24.19.0/SHASUMS256.txt`

`https://nodejs.org/dist/v24.19.0/SHASUMS256.txt.sig`

They are used only by offline tests. Production verification always downloads the manifest and its
detached signature from the release URL validated against the active Node.js distribution origin.
