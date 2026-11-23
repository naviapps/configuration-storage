#ifdef _WIN32
#include "configuration_storage.h"
#else
#include <CConfigurationStorage/configuration_storage.h>
#endif
#include <assert.h>
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { RESPONSE_CAPACITY = 1024 };

/* Quote only the caller-owned directory; the other configuration values are fixed. */
static char *configuration(const char *directory) {
    char *json = malloc(6 * strlen(directory) + 128);
    assert(json != NULL);
    const char prefix[] = "{\"directory\":\"";
    memcpy(json, prefix, sizeof prefix - 1);
    char *output = json + sizeof prefix - 1;
    for (const unsigned char *byte = (const unsigned char *)directory; *byte; ++byte) {
        if (*byte < 0x20) {
            int count = snprintf(output, 7, "\\u%04x", (unsigned int)*byte);
            assert(count == 6);
            output += 6;
        } else {
            if (*byte == '"' || *byte == '\\') {
                *output++ = '\\';
            }
            *output++ = (char)*byte;
        }
    }
    const char suffix[] =
        "/store\",\"owner\":\"com.example.standalone.storage\",\"maximumPayloadBytes\":4}";
    memcpy(output, suffix, sizeof suffix);
    return json;
}

static ConfigurationStorageStore *open_store(const char *config) {
    ConfigurationStorageResult *error = NULL;
    ConfigurationStorageStore *store =
        configuration_storage_store_open((const uint8_t *)config, strlen(config), &error);
    assert(store != NULL && error == NULL);
    return store;
}

static void dispatch(ConfigurationStorageStore *store, const char *command, uint32_t code,
                     char *response) {
    ConfigurationStorageResult *result =
        configuration_storage_store_dispatch(store, (const uint8_t *)command, strlen(command));
    assert(result != NULL);
    ConfigurationStorageResultView view = configuration_storage_result_view(result);
    assert(view.code == code && view.data != NULL && view.length < RESPONSE_CAPACITY);
    memcpy(response, view.data, view.length);
    response[view.length] = '\0';
    configuration_storage_result_destroy(result);
}

/* Inspect only known fields of SDK-generated snapshot and failure objects. */
static const char *field(const char *json, const char *name) {
    char key[64];
    int count = snprintf(key, sizeof key, "\"%s\"", name);
    assert(count > 0 && (size_t)count < sizeof key);
    const char *value = strstr(json, key);
    assert(value != NULL);
    value += count;
    while (isspace((unsigned char)*value)) {
        ++value;
    }
    assert(*value == ':');
    ++value;
    while (isspace((unsigned char)*value)) {
        ++value;
    }
    return value;
}

static void expect(const char *json, const char *name, const char *literal) {
    assert(strncmp(field(json, name), literal, strlen(literal)) == 0);
}

/* Keep the opaque generation's quoted JSON literal; never interpret or regenerate it. */
static void generation(const char *response, char *literal) {
    const char *start = field(response, "generation");
    assert(*start == '"');
    const char *end = start + 1;
    while (*end != '"' && *end != '\0') {
        if (*end++ == '\\') {
            assert(*end != '\0');
            ++end;
        }
    }
    assert(*end == '"');
    size_t count = (size_t)(end - start) + 1;
    assert(count < RESPONSE_CAPACITY);
    memcpy(literal, start, count);
    literal[count] = '\0';
}

static void save(ConfigurationStorageStore *store, const char *expected, const char *base64,
                 uint32_t code, char *response) {
    char command[RESPONSE_CAPACITY + 128];
    int count = snprintf(command, sizeof command,
                         "{\"command\":\"save\",\"expectedGeneration\":%s,\"payload\":\"%s\"}",
                         expected, base64);
    assert(count > 0 && (size_t)count < sizeof command);
    dispatch(store, command, code, response);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    assert(configuration_storage_abi_version() == CONFIGURATION_STORAGE_ABI_VERSION);
    char *config = configuration(argv[1]);
    ConfigurationStorageStore *writer = open_store(config);
    const char load[] = "{\"command\":\"load\"}";
    char response[RESPONSE_CAPACITY], first[RESPONSE_CAPACITY], latest[RESPONSE_CAPACITY];
    dispatch(writer, load, 0, response);
    expect(response, "generation", "null");
    expect(response, "payload", "null");

    /* Opaque bytes 00 ff 80 00, then ff 00 7f 01; no text decoding. */
    save(writer, "null", "AP+AAA==", 0, response);
    expect(response, "payload", "\"AP+AAA==\"");
    generation(response, first);
    ConfigurationStorageStore *reopened = open_store(config);
    free(config);
    dispatch(reopened, load, 0, response);
    expect(response, "generation", first);
    expect(response, "payload", "\"AP+AAA==\"");

    save(reopened, first, "/wB/AQ==", 0, response);
    expect(response, "payload", "\"/wB/AQ==\"");
    generation(response, latest);
    assert(strcmp(first, latest) != 0);
    save(writer, first, "b2xk", 1, response);
    expect(response, "code", "\"conflict\"");

    /* Five bytes exceed this handle's four-byte cap before generation or token checks. */
    save(writer, first, "c3RhbGU=", 1, response);
    expect(response, "code", "\"invalidConfiguration\"");
    dispatch(writer, "{\"command\":\"prepareRestore\",\"payload\":\"c3RhbGU=\"}", 1, response);
    expect(response, "code", "\"invalidConfiguration\"");
    dispatch(writer, "[\"restoreSnapshot\",\"c3RhbGU=\",\"stale\"]", 1, response);
    expect(response, "code", "\"invalidConfiguration\"");
    dispatch(writer, load, 0, response);
    expect(response, "generation", latest);
    expect(response, "payload", "\"/wB/AQ==\"");

    /* JSON escaping and the legacy positional form still accept four opaque bytes. */
    dispatch(writer, "[\"prepareRestore\",\"\\/wB/AQ==\"]", 0, response);
    assert(response[0] == '"');
    char restore[RESPONSE_CAPACITY + 64];
    int count =
        snprintf(restore, sizeof restore, "[\"restoreSnapshot\",\"/wB/AQ==\",%s]", response);
    assert(count > 0 && (size_t)count < sizeof restore);
    dispatch(writer, restore, 0, response);
    expect(response, "payload", "\"/wB/AQ==\"");
    generation(response, first);
    assert(strcmp(first, latest) != 0);

    configuration_storage_store_destroy(reopened);
    configuration_storage_store_destroy(writer);
    puts("Standalone installed C ConfigurationStorage consumer passed opaque bytes, generation "
         "conflict and bounded restore.");
    return 0;
}
