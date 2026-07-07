# Blobs and storage

`blob` is a first-class primitive for externally stored bytes.

## Descriptor

A blob descriptor carries content identity, byte length, media type, and optional name. The spec uses SHA-512 for end-to-end integrity.

## Placement

`$blob_storage` declares where bytes must live. Stores can be modeled as rows and chosen by policy.

## Commit gate

A commit that references a blob must only be admitted when the required blob bytes are available and verified. This keeps model history and external storage in sync.
