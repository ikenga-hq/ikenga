# Rotating the Secrets Key-Encryption Key (KEK)

This guide covers how to rotate the master key-encryption key (KEK) protecting user secrets on a multi-user Ikenga server.

## Overview

On a multi-user server, each person's secrets are encrypted with a unique data-encryption key (DEK) stored in an envelope file (`envelope.json`). Each envelope is wrapped under a per-user wrapping key derived from the server's master key-encryption key (`operator/secrets-kek`).

Rotating the KEK generates a fresh 256-bit server master key and re-wraps every user's envelope under it without modifying underlying secret values or ciphertexts.

## When to Rotate

Consider rotating the secrets KEK in scenarios such as:
- **Routine key hygiene**: Scheduled rotation aligned with your organization's security policy.
- **Suspected key exposure**: If the host filesystem, backup, or `operator/secrets-kek` file may have been compromised or accessed by unauthorized parties.
- **Instance decommissioning or migration**: Prior to reprovisioning, cloning, or migrating a managed multi-user server instance.

## What to Back Up First

Before running key rotation, create a complete backup of the server data directory:

1. **The operator directory**: `<data-dir>/operator/` contains `secrets-kek`, `accounts.db`, and server metadata.
2. **User data directories**: `<data-dir>/principals/` contains each user's data and encrypted secret envelopes (`principals/<id>/data/secrets/`).

Example backup command:
```bash
sudo tar -czvf ikenga-pre-rotation-backup.tar.gz /var/lib/ikenga/
```

> **Warning**: Keep the backup in a secure, root-accessible location. If `operator/secrets-kek` is lost without a backup before rotation completes, existing encrypted secrets cannot be decrypted.

## Stopping the Server

The rotation command enforces concurrency safety: it **refuses to run** while the server daemon or any user child session is active. This avoids race conditions with in-memory keys held by active processes.

Stop the server daemon before rotating:
```bash
sudo systemctl stop ikenga-server
```

Verify no processes are running:
```bash
pgrep -fl ikenga-server
```

## Running the Rotation Command

Run the `secrets rotate-kek` subcommand as `root`, pointing to your server's data root:

```bash
sudo ikenga-server secrets rotate-kek --data-dir /var/lib/ikenga
```

Or using the environment variable:
```bash
export IKENGA_DATA_DIR=/var/lib/ikenga
sudo -E ikenga-server secrets rotate-kek
```

### What Happens During Rotation

1. **Concurrency check**: Verifies that neither the server daemon nor any user child session is active.
2. **Journaling**: A crash-recovery journal (`operator/secrets-rotation-journal.json`) is initialized.
3. **Key generation**: A fresh 32-byte KEK is generated in `operator/secrets-kek.next`.
4. **Envelope rewrapping**: Each user's `envelope.json` is re-wrapped with the new key using secure, symlink-safe file operations. Progress is saved to the journal after each store.
5. **Full verification**: Before committing the new key, every user store is verified under the new KEK to confirm that the envelope unwraps and all stored secrets decrypt cleanly.
6. **Atomic activation**: The new key is moved to `operator/secrets-kek`, the old key is securely overwritten with zeros and unlinked, and the journal is removed.
7. **Audit logging**: The rotation is recorded in the server audit log with the timestamp, store count, and outcome (key material is never logged).

## Crash Recovery and Resumption

If the rotation process is interrupted mid-flight (for example, due to power failure or server restart):
- The server will automatically detect the journal file at startup and resume the rotation to completion before launching any user sessions.
- Alternatively, running `ikenga-server secrets rotate-kek` again will resume from the last completed store recorded in the journal.
