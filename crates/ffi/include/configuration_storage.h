#ifndef CONFIGURATION_STORAGE_H
#define CONFIGURATION_STORAGE_H
#include <stddef.h>
#include <stdint.h>
/* ABI 13 uses the ConfigurationStorage prefix for public C types.
 * Backup rotation requires a captured preparation token; the SQLite schema remains version 1. */
#define CONFIGURATION_STORAGE_ABI_VERSION 13U
#ifdef __cplusplus
extern "C" {
#endif
typedef struct ConfigurationStorageStore ConfigurationStorageStore;
typedef struct ConfigurationStorageResult ConfigurationStorageResult;
typedef struct ConfigurationStorageSaveQueue ConfigurationStorageSaveQueue;
typedef struct ConfigurationStorageResultView {
    uint32_t code;
    const uint8_t *data;
    size_t length;
} ConfigurationStorageResultView;
uint32_t configuration_storage_abi_version(void);
/* Valid readable bounded UTF-8 input. error is null or receives an owned failure.
 * Both opens perform disk I/O; hosts must schedule away from their UI executor. */
ConfigurationStorageStore *configuration_storage_store_open(const uint8_t *, size_t,
                                                            ConfigurationStorageResult **error);
/* Same configuration and ownership contract. Never creates or claims a store
 * or creates a lock for the returned handle. Only an absent root yields
 * storeMissing. Snapshot validation remains a separate operation. */
ConfigurationStorageStore *
configuration_storage_store_open_existing(const uint8_t *, size_t,
                                          ConfigurationStorageResult **error);
/* Store remains live. Separate dispatches may run concurrently; root contention
 * returns busy. A successful write remains committed even if a host task cancels.
 * save, prepareRestore and restoreSnapshot reject payloads beyond the store's
 * configured byte limit before base64 decoding or accessing store files.
 * prepareBackup {expectedGeneration} returns {backup:BackupSnapshot|null,token:string}.
 * Validate that exact opaque backup payload outside this call, then invoke
 * createBackup {expectedGeneration,token}. Its token is required even for an
 * absent backup or a same-generation no-op. A mismatched primary/backup raw revision
 * yields backupChanged before replacement; native link/type/limit checks still apply.
 * Preparation tokens cannot authorize rebuild or restore. Explicit rebuild remains
 * separate and retains old families. */
ConfigurationStorageResult *configuration_storage_store_dispatch(const ConfigurationStorageStore *,
                                                                 const uint8_t *, size_t);
/* Independent recovery-copy file I/O. Same owned result/view/destroy contract. */
ConfigurationStorageResult *configuration_storage_recovery_copy_dispatch(const uint8_t *, size_t);
/* Exclusive ownership; wait for all dispatch calls before destruction. */
void configuration_storage_store_destroy(ConfigurationStorageStore *);
/* Borrowed bytes expire at result destruction; copy before release. */
ConfigurationStorageResultView
configuration_storage_result_view(const ConfigurationStorageResult *);
void configuration_storage_result_destroy(ConfigurationStorageResult *);
/* Optional client write coalescing. No payload, timer, I/O, callback or retry policy.
 * The caller retains snapshot values and explicitly chooses when to begin writes. */
typedef struct {
    uint64_t revision, committed_revision, pending_revision, writing_revision, failed_revision;
    uint32_t has_unsaved_changes;
} ConfigurationStorageSaveQueueView;
ConfigurationStorageSaveQueue *configuration_storage_save_queue_create(void);
/* Zero on null/exhaustion; pending and in-flight work are unchanged. */
uint64_t configuration_storage_save_queue_enqueue(const ConfigurationStorageSaveQueue *);
/* Zero when no pending claim or a write is still in flight. */
uint64_t configuration_storage_save_queue_begin(const ConfigurationStorageSaveQueue *);
/* succeeded:0/1. Result:0 invalid,1 committed,2 current failure retained,
 * 3 failure superseded by a newer pending edit. Invalid input never advances. */
uint32_t configuration_storage_save_queue_complete(const ConfigurationStorageSaveQueue *,
                                                   uint64_t revision, uint32_t succeeded);
ConfigurationStorageSaveQueueView
configuration_storage_save_queue_view(const ConfigurationStorageSaveQueue *);
void configuration_storage_save_queue_destroy(ConfigurationStorageSaveQueue *);
#ifdef __cplusplus
}
#endif
#endif
