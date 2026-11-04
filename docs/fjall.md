# Fjall workspace storage

Fastrock embeds Fjall **3.1.13** through vwork. A workspace home now contains
`workspaces.fjall/`, rather than a single live database file. Fastrock remains
its sole process owner. Concurrent conversations use optimistic transactions
inside that owner; another Fastrock process cannot open the same directory.

## Compression and sharing

Fjall applies LZ4 to data blocks, indexes, journals, and separated large values.
Fastrock and vwork write ordinary serialized values. The only application-side
storage transform is standard Git COPY/INSERT delta encoding. Zstd remains only
as a legacy migration decoder. Existing archive import/export formats are not
changed by this storage migration.

Git content is stored once per typed Git object ID across repositories and
workspaces. New edits use that same object namespace. Workspaces keep independent
revision heads and immutable revision records, referencing shared content rather
than copying it. Legacy BLAKE3 edit records remain readable and shared by hash.
Delta encoding considers objects up to 16 MiB; larger objects use engine LZ4 alone.
Delta ancestry is bounded to four levels; reconstruction verifies lengths and
Git IDs. Garbage collection follows delta bases before removing unreachable
objects. Unreferenced edits are reclaimable; imported history remains retained.

Git headers, delta-base links and encoded lengths live in small immutable metadata
records, published atomically with their payloads. Git size checks, deduplication,
stats and GC inspect these records without inflating file contents. Key-only
scans/counts and deletion likewise avoid fetching large values.

Delta selection uses a bounded candidate index and a transaction-local cache.
Both use size/sample fingerprints to skip unrelated files before encoding.
Candidate entries and repository membership are independent append-only keys;
ordinary writes do not update a shared reference count, object count, or pack
manifest. Private Git refs have individual keys rather than rewriting one
repository record for every conversation. First publication of a new Git object
checks for a competing publisher; existing encodings remain immutable so
concurrent imports cannot create delta cycles. Different workspace heads and thread
summary generations have independent conflict domains. Same-workspace stale
writes still fail the revision guard. Fjall briefly coordinates commits and
journal durability; concurrent preparation does not imply parallel disk fsyncs.

Memory extraction remains a single idle-only worker. Its bounded memory snapshot
and user-edit generation are intentionally serialized with each other, in their
own keyspace; they do not lock workspace or summary transactions. Foreground
recall still reads its RAM cache only.

Fjall manages background compaction. Fastrock no longer runs periodic exclusive
whole-store compaction. Explicit maintenance remains available. Storage counters
called `compressed_*` by older clients now represent logical stored record bytes;
use directory disk usage for physical compression, including engine overhead.

## Migration

On first use, an existing `workspaces.redb` with no active Fjall store starts a
worker-side migration. The native progress panel shows copying, Git delta
optimization, verification, and final activation. Slint remains responsive.
Session startup waits for migration instead of expiring its ordinary setup timer.

Migration copies bounded batches into `workspaces.fjall.migrating/data/`, with
transactional progress cursors. A pause or crash resumes from committed batches.
The destination is activated only after comparing every logical record and table
count with the source and flushing the new database. The original redb is never
rewritten, repaired in place, renamed, or deleted. If redb needs recovery, only a
locked private copy is repaired. Changed sources, unknown schemas/tables,
corruption, existing live destinations, and ownership conflicts fail closed.
Keep the source backup until satisfied with the migrated application.

**File → Migrate legacy storage…** selects another redb file and an unused
workspace home, or resumes a paused migration. Completed manual imports appear
in the Virtual workspace browser. This registers the home without changing
provider configuration. It does not silently retarget existing conversation
bindings. Pause remains paused for the running application until Resume; restart
can resume automatic migration. A completed destination is never overwritten.

Normal later startup opens Fjall directly, without reading or validating the old
redb again. Back up the whole Fjall directory while Fastrock is stopped; copying
a subset of an active database is not a backup.

## Cold transcript pages

Large histories use indexed pages in a separate disposable Fjall cache, with LZ4
configured in the engine, a bounded block/write cache and a bounded decoded
viewport cache. Provider history remains authoritative. Closed pages are removed;
normal shutdown deletes the temporary cache. Old crash leftovers are reclaimed
only after checking age, the exact cache marker, and exclusive ownership. A cache
write failure retains raw in-memory content rather than losing the transcript.

## Verification

The vwork suite covers overlapping transaction preparation, revision conflicts,
Git delta round trips/depth bounds, cross-repository delta roots, dead overlay
collection, first-batch imports, competing object publication, interrupted migrations, dirty legacy recovery,
source preservation, and refusal to overwrite live stores. Fastrock adds native
migration controls and regressions for summaries, memories, checkpoints,
scrollback, and multiple streaming conversations. Native tests run on `games`;
Mac validation compiles only. Benchmarks cap Fastrock at 32 GB and reduce workload
tenfold when the guard stops a run.
