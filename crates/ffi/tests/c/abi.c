#include "configuration_storage.h"
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <direct.h>
#include <io.h>
#else
#include <signal.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

/* The driver supplies a fresh disposable directory. Never use a product store. */
static char *copy_result(ConfigurationStorageResult *result, uint32_t code) {
    assert(result != NULL);
    ConfigurationStorageResultView view = configuration_storage_result_view(result);
    if (view.code != code && view.data != NULL) {
        int count = view.length < 1024 ? (int)view.length : 1024;
        fprintf(stderr, "Expected result code %u, received %u: %.*s\n", (unsigned int)code,
                (unsigned int)view.code, count, (const char *)view.data);
    }
    assert(view.code == code && view.data != NULL && view.length > 0);
    assert(view.length <= 96u * 1024u * 1024u);
    char *copy = (char *)malloc(view.length + 1);
    assert(copy != NULL);
    memcpy(copy, view.data, view.length);
    copy[view.length] = '\0';
    configuration_storage_result_destroy(result);
    return copy;
}

static ConfigurationStorageResult *dispatch(ConfigurationStorageStore *store, const char *command) {
    return configuration_storage_store_dispatch(store, (const uint8_t *)command, strlen(command));
}

static char *success(ConfigurationStorageStore *store, const char *command) {
    ConfigurationStorageResult *result = dispatch(store, command);
    if (result != NULL && configuration_storage_result_view(result).code != 0)
        fprintf(stderr, "Failed owned test command: %s\n", command);
    return copy_result(result, 0);
}

static void failure(ConfigurationStorageResult *result, const char *code) {
    char *json = copy_result(result, 1);
    char expected[128];
    int n = snprintf(expected, sizeof expected, "\"code\":\"%s\"", code);
    assert(n > 0 && (size_t)n < sizeof expected && strstr(json, expected) != NULL);
    free(json);
}

/* Only extract ABI-generated opaque generation/base64/token fields, never arbitrary JSON. */
static void field(const char *json, const char *name, char *output, size_t capacity) {
    char key[128];
    int n = snprintf(key, sizeof key, "\"%s\":\"", name);
    assert(n > 0 && (size_t)n < sizeof key);
    const char *start = strstr(json, key);
    assert(start != NULL);
    start += strlen(key);
    const char *end = strchr(start, '"');
    assert(end != NULL && (size_t)(end - start) < capacity);
    memcpy(output, start, (size_t)(end - start));
    output[end - start] = '\0';
}

static char *prepare_backup(ConfigurationStorageStore *store, const char *generation, char *token,
                            size_t capacity) {
    char command[256];
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"prepareBackup\",\"expectedGeneration\":\"%s\"}", generation);
    char *json = success(store, command);
    /* A nested backup token restores; the final field authorizes rotation. */
    const char *outer = strstr(json, "\"token\":\"");
    assert(outer != NULL);
    for (const char *next = strstr(outer + 1, "\"token\":\""); next != NULL;
         next = strstr(outer + 1, "\"token\":\""))
        outer = next;
    field(outer, "token", token, capacity);
    return json;
}

static char *configuration(const char *directory, const char *owner) {
    size_t length = strlen(directory);
    char *json = (char *)malloc(length * 6 + 256);
    assert(json != NULL);
    char *cursor = json;
    const char *prefix = "{\"directory\":\"";
    memcpy(cursor, prefix, strlen(prefix));
    cursor += strlen(prefix);
    for (const unsigned char *p = (const unsigned char *)directory; *p; p++) {
        if (*p == '\\' || *p == '"') {
            *cursor++ = '\\';
            *cursor++ = (char)*p;
        } else if (*p < 32) {
            int n = sprintf(cursor, "\\u%04x", (unsigned int)*p);
            cursor += n;
        } else {
            *cursor++ = (char)*p;
        }
    }
    (void)sprintf(cursor, "\",\"owner\":\"%s\"}", owner);
    return json;
}

static char *child_path(const char *directory, const char *name) {
    size_t capacity = strlen(directory) + strlen(name) + 2;
    char *path = (char *)malloc(capacity);
    assert(path != NULL);
    int n = snprintf(path, capacity, "%s/%s", directory, name);
    assert(n > 0 && (size_t)n < capacity);
    return path;
}

static void assert_missing_path(const char *path) {
#ifdef _WIN32
    assert(_access(path, 0) == -1 && errno == ENOENT);
#else
    assert(access(path, F_OK) == -1 && errno == ENOENT);
#endif
}

static ConfigurationStorageStore *open_existing(const char *config,
                                                ConfigurationStorageResult **error) {
    return configuration_storage_store_open_existing((const uint8_t *)config, strlen(config),
                                                     error);
}

static void existing_store_contract(const char *directory) {
    ConfigurationStorageResult *error = NULL;
    assert(configuration_storage_store_open_existing(NULL, 0, &error) == NULL);
    failure(error, "invalidConfiguration");
    const uint8_t invalid_utf8[] = {0xff};
    assert(configuration_storage_store_open_existing(invalid_utf8, sizeof invalid_utf8, &error) ==
           NULL);
    failure(error, "invalidConfiguration");
    assert(open_existing("{}", NULL) == NULL);

    char *path = child_path(directory, "existing-store");
    char *config = configuration(path, "c-test.owner");
    assert(open_existing(config, &error) == NULL);
    failure(error, "storeMissing");
    assert(open_existing(config, NULL) == NULL);
    assert_missing_path(path);

#ifdef _WIN32
    assert(_mkdir(path) == 0);
#else
    assert(mkdir(path, 0700) == 0);
#endif
    char *owner = child_path(path, "store-owner");
    char *lock = child_path(path, "store.lock");
    assert(open_existing(config, &error) == NULL);
    failure(error, "ownerMismatch");
    assert_missing_path(owner);
    assert_missing_path(lock);
    free(owner);

    ConfigurationStorageStore *store =
        configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
    assert(store != NULL && error == NULL);
    char *json =
        success(store, "{\"command\":\"save\",\"expectedGeneration\":null,\"payload\":\"AP8q\"}");
    free(json);
    configuration_storage_store_destroy(store);

    char *foreign = configuration(path, "foreign-owner");
    assert(open_existing(foreign, &error) == NULL);
    ConfigurationStorageResult *foreign_error = error;
    free(foreign);
    store = open_existing(config, &error);
    assert(store != NULL && error == NULL);
    failure(foreign_error, "ownerMismatch");
    json = success(store, "{\"command\":\"load\"}");
    assert(strstr(json, "\"payload\":\"AP8q\"") != NULL);
    free(json);
    configuration_storage_store_destroy(store);

    /* An owned but damaged store is admitted for separate load/recovery decisions. */
    char *selection = child_path(path, "active-generation");
    const char corruption[] = "not-a-generation";
    FILE *marker = fopen(selection, "wb");
    assert(marker != NULL);
    assert(fwrite(corruption, 1, sizeof corruption - 1, marker) == sizeof corruption - 1);
    assert(fclose(marker) == 0);
    store = open_existing(config, &error);
    assert(store != NULL && error == NULL);
    failure(dispatch(store, "{\"command\":\"load\"}"), "invalidStoredData");
    marker = fopen(selection, "rb");
    assert(marker != NULL);
    char persisted[sizeof corruption];
    assert(fread(persisted, 1, sizeof persisted, marker) == sizeof corruption - 1);
    assert(memcmp(persisted, corruption, sizeof corruption - 1) == 0);
    assert(fclose(marker) == 0);

    /* Commands on an existing-only handle must not recreate a removed lock. */
    assert(remove(lock) == 0);
    failure(dispatch(store, "{\"command\":\"load\"}"), "invalidStoredData");
    assert_missing_path(lock);
    configuration_storage_store_destroy(store);
    free(selection);
    free(lock);
    free(config);
    free(path);
}

#ifndef _WIN32
static void owner_publication_retry(const char *directory, rlim_t limit) {
    size_t capacity = strlen(directory) + 64;
    char *path = (char *)malloc(capacity);
    assert(path != NULL);
    int n = snprintf(path, capacity, "%s/c-owner-limit-%lu", directory, (unsigned long)limit);
    assert(n > 0 && (size_t)n < capacity);
    char *config = configuration(path, "c-test.owner");
    free(path);

    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        /* Only this child has constrained writes; the parent retains its limits. */
        struct rlimit file_size = {limit, limit};
        assert(signal(SIGXFSZ, SIG_IGN) != SIG_ERR);
        assert(setrlimit(RLIMIT_FSIZE, &file_size) == 0);
        ConfigurationStorageResult *error = NULL;
        ConfigurationStorageStore *store =
            configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
        assert(store == NULL);
        failure(error, "ioFailure");
        _exit(0);
    }

    int status;
    pid_t waited;
    do {
        waited = waitpid(child, &status, 0);
    } while (waited < 0 && errno == EINTR);
    assert(waited == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);

    /* Failed zero/partial writes must leave the same directory claimable. */
    ConfigurationStorageResult *error = NULL;
    ConfigurationStorageStore *store =
        configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
    assert(store != NULL && error == NULL);
    free(config);
    char *json = success(store, "{\"command\":\"load\"}");
    assert(strstr(json, "\"generation\":null") && strstr(json, "\"payload\":null"));
    free(json);
    configuration_storage_store_destroy(store);
}
#endif

static void save_queue_contract(void) {
    ConfigurationStorageSaveQueue *queue = configuration_storage_save_queue_create();
    assert(queue);
    assert(configuration_storage_save_queue_enqueue(queue) == 1);
    assert(configuration_storage_save_queue_enqueue(queue) == 2);
    assert(configuration_storage_save_queue_begin(queue) == 2);
    assert(configuration_storage_save_queue_enqueue(queue) == 3);
    ConfigurationStorageSaveQueueView writing = configuration_storage_save_queue_view(queue);
    assert(writing.revision == 3 && !writing.committed_revision && writing.pending_revision == 3 &&
           writing.writing_revision == 2 && !writing.failed_revision &&
           writing.has_unsaved_changes == 1);
    assert(configuration_storage_save_queue_complete(queue, 2, 1) == 1);
    assert(configuration_storage_save_queue_begin(queue) == 3);
    assert(configuration_storage_save_queue_complete(queue, 3, 1) == 1);
    ConfigurationStorageSaveQueueView saved = configuration_storage_save_queue_view(queue);
    assert(saved.revision == 3 && saved.committed_revision == 3 && !saved.pending_revision &&
           !saved.writing_revision && !saved.failed_revision && !saved.has_unsaved_changes);
    configuration_storage_save_queue_destroy(queue);
}

static void recovery_copy_contract(ConfigurationStorageStore *store, const char *directory) {
    failure(configuration_storage_recovery_copy_dispatch(NULL, 0), "invalidConfiguration");
    size_t capacity = strlen(directory) * 6 + 512;
    char *path = (char *)malloc(strlen(directory) + 64);
    assert(path != NULL);
    (void)sprintf(path, "%s/portable-recovery.bin", directory);
    char *escaped = configuration(path, "unused");
    char *suffix = strstr(escaped, ",\"owner\"");
    assert(suffix != NULL);
    *suffix = '\0';
    const char *value = strchr(escaped, ':') + 1;
    char *command = (char *)malloc(capacity);
    assert(command != NULL);
    int n = snprintf(
        command, capacity,
        "{\"command\":\"write\",\"path\":%s,\"maximumPayloadBytes\":64,\"payload\":\"AP8q\"}",
        value);
    assert(n > 0 && (size_t)n < capacity);
    char *json = copy_result(
        configuration_storage_recovery_copy_dispatch((const uint8_t *)command, strlen(command)), 0);
    assert(strcmp(json, "null") == 0);
    free(json);
    n = snprintf(command, capacity, "{\"command\":\"read\",\"path\":%s,\"maximumPayloadBytes\":64}",
                 value);
    assert(n > 0 && (size_t)n < capacity);
    json = copy_result(
        configuration_storage_recovery_copy_dispatch((const uint8_t *)command, strlen(command)), 0);
    assert(strstr(json, "\"payload\":\"AP8q\"") && strstr(json, "\"modifiedAt\":"));
    char digest[65];
    field(json, "digest", digest, sizeof digest);
    assert(strlen(digest) == 64);
    free(json);
    json = success(store, "{\"command\":\"prepareRestore\",\"payload\":\"bmV3\"}");
    assert(strlen(json) == 66 && json[0] == '"' && json[65] == '"');
    char token[65];
    memcpy(token, json + 1, 64);
    token[64] = '\0';
    free(json);
    n = snprintf(command, capacity,
                 "{\"command\":\"restoreSnapshot\",\"payload\":\"bmV3\",\"token\":\"%s\"}", token);
    assert(n > 0 && (size_t)n < capacity);
    json = success(store, command);
    assert(strstr(json, "\"payload\":\"bmV3\""));
    free(json);
    failure(dispatch(store, command), "restoreChanged");
    FILE *file = fopen(path, "r+b");
    assert(file != NULL);
    assert(fputc('X', file) != EOF);
    assert(fclose(file) == 0);
    n = snprintf(command, capacity, "{\"command\":\"read\",\"path\":%s,\"maximumPayloadBytes\":64}",
                 value);
    assert(n > 0 && (size_t)n < capacity);
    failure(configuration_storage_recovery_copy_dispatch((const uint8_t *)command, strlen(command)),
            "invalidRecoveryCopy");
    free(command);
    free(escaped);
    free(path);
}

static void snapshot_candidates_contract(ConfigurationStorageStore *store) {
    char *json =
        success(store, "{\"command\":\"listSnapshotCandidates\",\"limit\":1,\"after\":null}");
    char id[65];
    field(json, "id", id, sizeof id);
    char source[32];
    field(json, "source", source, sizeof source);
    assert((strcmp(source, "backup") == 0 || strcmp(source, "generation") == 0) &&
           strstr(json, "\"isSelected\":") != NULL);
    free(json);
    char command[256];
    int count = snprintf(command, sizeof command,
                         "{\"command\":\"readSnapshotCandidate\",\"id\":\"%s\"}", id);
    assert(count > 0 && (size_t)count < sizeof command);
    json = success(store, command);
    char read_id[65];
    field(json, "id", read_id, sizeof read_id);
    char read_source[32];
    field(json, "source", read_source, sizeof read_source);
    assert(strcmp(source, read_source) == 0 && strcmp(id, read_id) == 0 &&
           strstr(json, "\"snapshot\":{") != NULL && strstr(json, "\"modifiedAt\":") != NULL);
    free(json);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    assert(configuration_storage_abi_version() == CONFIGURATION_STORAGE_ABI_VERSION);
    ConfigurationStorageResultView missing = configuration_storage_result_view(NULL);
    assert(missing.code != 0 && missing.data == NULL && missing.length == 0);
    configuration_storage_result_destroy(NULL);
    configuration_storage_store_destroy(NULL);
    ConfigurationStorageResult *error = NULL;
    assert(configuration_storage_store_open(NULL, 0, &error) == NULL);
    failure(error, "invalidConfiguration");
    const uint8_t invalid_utf8[] = {0xff};
    assert(configuration_storage_store_open(invalid_utf8, sizeof invalid_utf8, &error) == NULL);
    failure(error, "invalidConfiguration");
    assert(configuration_storage_store_open((const uint8_t *)"{}", 2, NULL) == NULL);
    char *config = configuration(argv[1], "c-test.owner");
    ConfigurationStorageStore *store =
        configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
    assert(store != NULL && error == NULL);
    free(config);

#ifndef _WIN32
    /* Claim the outer fixture before adding subdirectories; fork before queue work. */
    owner_publication_retry(argv[1], 0);
    owner_publication_retry(argv[1], 4);
#endif
    existing_store_contract(argv[1]);
    save_queue_contract();

    char *json = success(store, "{\"command\":\"load\"}");
    assert(strstr(json, "\"generation\":null") && strstr(json, "\"payload\":null"));
    free(json);
    failure(dispatch(store, "{\"command\":\"readBackup\"}"), "backupMissing");
    failure(dispatch(store, "{\"command\":\"prepareBackup\",\"expectedGeneration\":\"12345678-1234-"
                            "4321-8123-123456789abc\"}"),
            "notInitialized");
    failure(configuration_storage_store_dispatch(NULL, (const uint8_t *)"{}", 2),
            "invalidConfiguration");
    failure(configuration_storage_store_dispatch(store, NULL, 0), "invalidConfiguration");
    /* The oversized length is rejected before dereferencing the readable fixture. */
    failure(
        configuration_storage_store_dispatch(store, (const uint8_t *)"{}", 96u * 1024u * 1024u + 1),
        "invalidConfiguration");
    const char *malformed[] = {
        "{",
        "{}",
        "{\"command\":\"unknown\"}",
        "{\"command\":false}",
        "{\"command\":\"save\",\"expectedGeneration\":null,\"payload\":\"!\"}",
        "{\"command\":\"prepareBackup\"}",
        "{\"command\":\"createBackup\",\"expectedGeneration\":\"unknown\"}",
        "{\"command\":\"createBackup\",\"expectedGeneration\":\"unknown\",\"token\":null}"};
    for (size_t i = 0; i < sizeof malformed / sizeof malformed[0]; i++)
        failure(dispatch(store, malformed[i]), "invalidConfiguration");

    ConfigurationStorageResult *owned =
        dispatch(store, "{\"command\":\"save\",\"expectedGeneration\":null,\"payload\":\"AP8q\"}");
    ConfigurationStorageResultView borrowed = configuration_storage_result_view(owned);
    assert(borrowed.code == 0 && borrowed.data != NULL && borrowed.length > 0);
    json = success(store, "{\"command\":\"load\"}");
    assert(strlen(json) == borrowed.length && memcmp(json, borrowed.data, borrowed.length) == 0);
    free(json);
    json = copy_result(owned, 0);
    char first[128], second[128], restored[128], token[128], changed[128], rotation[128],
        command[512];
    field(json, "generation", first, sizeof first);
    assert(strstr(json, "\"payload\":\"AP8q\""));
    free(json);
    json = prepare_backup(store, first, rotation, sizeof rotation);
    assert(strstr(json, "\"backup\":null"));
    free(json);
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"createBackup\",\"expectedGeneration\":\"%s\",\"token\":\"%s\"}",
                   first, rotation);
    json = success(store, command);
    field(json, "token", token, sizeof token);
    assert(strstr(json, "\"snapshot\":{") && strstr(json, "\"modifiedAt\":"));
    free(json);
    failure(dispatch(store, command), "backupChanged");
    json = prepare_backup(store, first, rotation, sizeof rotation);
    assert(strstr(json, "\"backup\":{") && strstr(json, "\"payload\":\"AP8q\""));
    assert(strcmp(rotation, token) != 0);
    free(json);
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"createBackup\",\"expectedGeneration\":\"%s\",\"token\":\"%s\"}",
                   first, token);
    failure(dispatch(store, command), "backupChanged");
    json = success(store, "{\"command\":\"readBackup\"}");
    field(json, "token", changed, sizeof changed);
    assert(strcmp(token, changed) == 0);
    free(json);

    (void)snprintf(command, sizeof command,
                   "{\"command\":\"save\",\"expectedGeneration\":\"%s\",\"payload\":\"\"}", first);
    json = success(store, command);
    field(json, "generation", second, sizeof second);
    assert(strcmp(first, second) != 0 && strstr(json, "\"payload\":\"\""));
    free(json);
    failure(dispatch(store, command), "conflict");
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"createBackup\",\"expectedGeneration\":\"%s\",\"token\":\"%s\"}",
                   first, rotation);
    failure(dispatch(store, command), "backupChanged");
    (void)snprintf(command, sizeof command, "{\"command\":\"restoreBackup\",\"token\":\"%s\"}",
                   token);
    failure(dispatch(store, command), "backupChanged");
    /* Host validation must use a new token after any active selection change. */
    json = success(store, "{\"command\":\"readBackup\"}");
    field(json, "token", changed, sizeof changed);
    assert(strcmp(changed, token) != 0 && strstr(json, "\"payload\":\"AP8q\""));
    free(json);
    (void)snprintf(command, sizeof command, "{\"command\":\"restoreBackup\",\"token\":\"%s\"}",
                   changed);
    json = success(store, command);
    field(json, "generation", restored, sizeof restored);
    assert(strcmp(restored, first) != 0 && strcmp(restored, second) != 0 &&
           strstr(json, "\"payload\":\"AP8q\""));
    free(json);
    json = prepare_backup(store, restored, rotation, sizeof rotation);
    assert(strstr(json, "\"backup\":{") && strstr(json, "\"payload\":\"AP8q\""));
    free(json);
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"createBackup\",\"expectedGeneration\":\"%s\",\"token\":\"%s\"}",
                   restored, rotation);
    json = success(store, command);
    field(json, "token", changed, sizeof changed);
    assert(strcmp(changed, token) != 0);
    free(json);
    (void)snprintf(command, sizeof command, "{\"command\":\"restoreBackup\",\"token\":\"%s\"}",
                   token);
    failure(dispatch(store, command), "backupChanged");
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"prepareBackupRebuild\",\"expectedGeneration\":\"%s\"}",
                   restored);
    json = success(store, command);
    assert(strlen(json) == 66 && json[0] == '"' && json[65] == '"');
    memcpy(token, json + 1, 64);
    token[64] = '\0';
    free(json);
    (void)snprintf(command, sizeof command,
                   "{\"command\":\"rebuildBackup\",\"expectedGeneration\":\"%s\",\"token\":\"%s\"}",
                   restored, token);
    json = success(store, command);
    assert(strstr(json, restored) && strstr(json, "\"payload\":\"AP8q\""));
    free(json);
    failure(dispatch(store, command), "backupChanged");
    config = configuration(argv[1], "foreign-owner");
    assert(configuration_storage_store_open((const uint8_t *)config, strlen(config), &error) ==
           NULL);
    failure(error, "ownerMismatch");
    free(config);
    configuration_storage_store_destroy(store);

    /* Independent reopen proves the owner and opaque persisted payload survived. */
    config = configuration(argv[1], "c-test.owner");
    store = configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
    free(config);
    assert(store != NULL && error == NULL);
    json = success(store, "{\"command\":\"load\"}");
    assert(strstr(json, restored) && strstr(json, "\"payload\":\"AP8q\""));
    recovery_copy_contract(store, argv[1]);
    snapshot_candidates_contract(store);
    configuration_storage_store_destroy(store);
    assert(strstr(json, restored)); /* Host copy survives result and store owners. */
    free(json);
    puts("C owned-tempstore ABI/ownership/backup/recovery-copy/guarded-restore checks passed.");
    return 0;
}
