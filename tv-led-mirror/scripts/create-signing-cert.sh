#!/usr/bin/env bash
#
# One-time setup: generate a self-signed code-signing certificate in the
# user's login keychain so `bundle.sh` can sign with a stable identity.
#
# Why this matters
# ----------------
# macOS TCC (Privacy & Security permissions) keys grants on the tuple of
#   (bundle ID, designated requirement)
# where the designated requirement is derived from the code-signing cert.
#
# Ad-hoc signing (`codesign --sign -`) gives every binary a fresh cdhash —
# macOS treats every rebuild as a NEW app, prompting for Screen Recording
# again. Worse, if the user already granted permission, the new build's
# different cdhash makes macOS silently DENY screen capture (you see a black
# screen / "permission denied" rather than a re-prompt) until the old entry
# is removed from System Settings.
#
# Signing with a self-signed cert that lives in your login keychain gives
# every build the SAME designated requirement. macOS recognizes rebuilds as
# the same app, and TCC grants persist across `bundle.sh` runs.
#
# Run this once. The cert is valid for 10 years. To remove later:
#   security delete-certificate -c "TVLEDMirror Code Signing"
set -euo pipefail

CERT_NAME="TVLEDMirror Code Signing"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

# Bail early if the cert is already installed — re-running is harmless but
# we skip the work + keychain prompt.
if security find-identity -v -p codesigning 2>/dev/null | grep -q "$CERT_NAME"; then
    echo "Code-signing cert '$CERT_NAME' already present. Nothing to do."
    echo "Re-run scripts/bundle.sh to use it."
    exit 0
fi

# Build the cert in a temp dir we wipe on exit. The intermediate PEM files
# briefly contain a private key — keep them out of $ROOT.
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Minimal openssl config: codeSigning EKU is the part that matters for
# Apple's codesign to accept the cert. basicConstraints CA:false avoids the
# cert being treated as a CA. keyUsage digitalSignature is required.
cat > "$TMPDIR/cert.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = $CERT_NAME
O = Latent Space (self-signed)
[v3]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

echo "→ Generating self-signed cert (10 years, RSA 2048)"
openssl req -new -x509 -days 3650 -nodes \
    -newkey rsa:2048 \
    -keyout "$TMPDIR/key.pem" \
    -out "$TMPDIR/cert.pem" \
    -config "$TMPDIR/cert.cnf" \
    -extensions v3 \
    2>/dev/null

# Bundle key + cert into PKCS12 because that's what `security import` expects
# for a cert+key pair. Empty passphrase is fine — the key is sandboxed inside
# the login keychain anyway and we pre-authorize codesign to use it below.
#
# `-legacy` is REQUIRED on openssl 3.x: the new default KDF (PBKDF2 with a
# higher iteration count) produces PKCS12 files that Apple's `security`
# command can't decrypt — it fails with "MAC verification failed". The legacy
# RC2/3DES PKCS12 v1 format is what macOS still expects.
echo "→ Packaging into PKCS12"
openssl pkcs12 -export -legacy \
    -inkey "$TMPDIR/key.pem" \
    -in "$TMPDIR/cert.pem" \
    -out "$TMPDIR/cert.p12" \
    -passout pass:tvledmirror \
    -name "$CERT_NAME"

# `-T /usr/bin/codesign` pre-authorizes codesign to use the private key
# without an interactive prompt on first sign. Without this, every fresh
# bundle.sh run pops a "codesign wants to sign with key" dialog.
echo "→ Importing into login keychain"
security import "$TMPDIR/cert.p12" \
    -k "$KEYCHAIN" \
    -P tvledmirror \
    -T /usr/bin/codesign \
    -T /usr/bin/security

# Trust the cert for codeSigning. This step needs admin (sudo) to write into
# the system trust store on stricter macOS versions; without it codesign
# may reject the cert as "not trusted for code signing". On personal-use
# Macs the keychain trust is usually enough — try without sudo first.
echo "→ Marking cert trusted for code signing"
if security add-trusted-cert -d -r trustRoot -p codeSign \
        -k "$KEYCHAIN" "$TMPDIR/cert.pem" 2>/dev/null; then
    echo "  user-keychain trust added"
else
    echo "  (skipped user-keychain trust step — codesign will likely still"
    echo "   accept the cert; if it errors with 'unable to build chain to"
    echo "   self-signed root', re-run with sudo)"
fi

echo
echo "Done. Verify with:"
echo "  security find-identity -v -p codesigning | grep '$CERT_NAME'"
echo
echo "Then build the app:"
echo "  scripts/bundle.sh"
echo
echo "First build will prompt for screen recording permission. Subsequent"
echo "builds will inherit the grant — no re-prompt, no need to remove the"
echo "old entry from System Settings."
