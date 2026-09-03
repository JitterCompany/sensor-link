# YubiKey CA setup

One-time procedure to load a project CA private key onto a YubiKey so
`sensor-link-provision` can sign device certificates with it. Works for any
YubiKey with the PIV applet (5 series, incl. 5C/5C NFC; 4 series also works).

The key goes into a **PIV retired slot** (default R1 = `82`, set per project
by `ca_piv_slot` in `provision.toml`) and the CA certificate is stored in the
same slot so the production PC needs no separate copy. The retired slots
(82–95) are functionally identical to the conventional 9a/9c/9d slots and
using one leaves those free.

The tool signs through PC/SC directly; none of the OpenSSL/PKCS#11 tooling
some older flows needed (libykcs11, pkcs11 engine, opensc) is required.

## Warnings before you start

- **One-way door.** A private key imported into a PIV slot cannot be read
  back out. The copy in the secrets store is the *only* backup: never delete
  it. If the YubiKey is lost or dies, import the backup into a replacement;
  device certificates already in the field stay valid either way.
- **Algorithm.** The tool signs with ECDSA **P-256**; the CA key must be a
  prime256v1 key. Check before importing (step 3).
- **PIN/PUK lockout.** 3 wrong PIN attempts lock the PIN (unblock with the
  PUK); 3 wrong PUK attempts brick the PIV applet permanently.
- **Plaintext key handling.** `ykman piv keys import` reads unencrypted PEM.
  If the backup is encrypted, decrypt straight into the pipe; do not leave a
  plaintext copy behind.

## 1. Install ykman

Only needed for this setup, not for provisioning itself.

```sh
# macOS
brew install ykman
# Debian/Ubuntu (pcscd is also the tool's runtime dependency)
sudo apt install -y yubikey-manager pcscd
sudo systemctl enable --now pcscd
```

## 2. Confirm the YubiKey is visible

```sh
ykman info
ykman piv info
```

`piv info` should print the PIV firmware version and (on a fresh key) no
populated slots. On Linux, if nothing is found: `sudo systemctl restart
pcscd` and replug.

## 3. Check the CA key algorithm

```sh
<decrypt-or-cat CA key> | openssl ec -noout -text 2>/dev/null | grep ASN1
```

Expected: `ASN1 OID: prime256v1`. Anything else cannot be used by the tool;
stop here.

## 4. Harden the YubiKey (fresh keys only)

Skip if this YubiKey is already hardened and in use elsewhere.

```sh
# Management key: generate on-card, protected by the PIN, so admin
# operations only ever need the PIN from now on.
ykman piv access change-management-key --generate --protect

# PIV PIN (factory default 123456), 6-8 digits.
ykman piv access change-pin

# PUK (factory default 12345678), 6-8 digits; unblocks the PIN after
# 3 wrong attempts.
ykman piv access change-puk
```

Record the new PIN and PUK in the same secure store that holds the CA
backup. Losing the PUK means 3 wrong PIN entries permanently lock the key.

## 5. Import the CA private key

Into the slot from the project's `provision.toml` (`R1` = `82`):

```sh
<decrypt-or-cat CA key> | ykman piv keys import \
    --pin-policy once \
    --touch-policy never \
    82 -
```

- `--pin-policy once`: the PIN is entered once per session, which is exactly
  what the tool does at *Start session*.
- `--touch-policy never`: no tap per device, matching the scan-and-go
  production flow. Use `cached` instead if you want a physical confirmation
  (one tap authorises ~15 s of signing); the tool's per-device signing then
  blocks until the key is tapped when it blinks.
- The trailing `-` reads the key from stdin.

## 6. Import the CA certificate

The certificate is public; a file on disk is fine. Storing it on the key
lets the tool read it at session start, so the production PC needs no CA
file at all:

```sh
ykman piv certificates import 82 /path/to/ca.pem
```

(`ykman piv certificates export 82 -` prints it back later.)

## 7. Verify

```sh
ykman piv info          # slot 82 populated, ECCP256, policies as chosen
```

Then the end-to-end check through the exact code path production uses: sign
and chain-verify a test certificate (asks for the PIN; needs any release zip
of the project for its profile):

```sh
sensor-link-provision --selftest-sign --zip firmware-build-<run>.zip
```

The same check also runs automatically at every *Start session* in the GUI,
before any device is touched.

## Replacement / spare key

Run sections 2 and 4–7 on the spare, importing the same CA key from the
backup. Keeping a second, already-imported YubiKey means a lost or broken
key never stops production.
