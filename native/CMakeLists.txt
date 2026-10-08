# 配置 Android x86_64 专用加载器、固定第三方依赖以及可分离的发布符号产物。

cmake_minimum_required(VERSION 3.21)

project(azlw_native_runtime VERSION 0.11.0 LANGUAGES C CXX)

option(AZLW_BUILD_TESTS "Build native runtime validation executables" ON)

set(AZLW_RUNTIME_RPC_CONTRACT "${CMAKE_CURRENT_SOURCE_DIR}/../contracts/runtime-rpc-v1.json")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${AZLW_RUNTIME_RPC_CONTRACT}")
file(READ "${AZLW_RUNTIME_RPC_CONTRACT}" AZLW_RUNTIME_RPC_CONTRACT_JSON)

set(AZLW_SECURE_CHANNEL_CONTRACT
    "${CMAKE_CURRENT_SOURCE_DIR}/../crates/secure-channel/secure-channel-v1.json")
set_property(DIRECTORY APPEND PROPERTY CMAKE_CONFIGURE_DEPENDS "${AZLW_SECURE_CHANNEL_CONTRACT}")
file(READ "${AZLW_SECURE_CHANNEL_CONTRACT}" AZLW_SECURE_CHANNEL_CONTRACT_JSON)

string(JSON AZLW_CONTRACT_SCHEMA_VERSION
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" schema_version)
if(NOT AZLW_CONTRACT_SCHEMA_VERSION EQUAL 1)
    message(FATAL_ERROR
        "unsupported runtime RPC contract schema version ${AZLW_CONTRACT_SCHEMA_VERSION}")
endif()
string(JSON AZLW_CONTRACT_PROTOCOL_VERSION
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants protocol_version)
string(JSON AZLW_CONTRACT_AGENT_VERSION
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants agent_version)
if(NOT "${PROJECT_VERSION}" STREQUAL "${AZLW_CONTRACT_AGENT_VERSION}")
    message(FATAL_ERROR
        "native project version ${PROJECT_VERSION} does not match runtime RPC contract agent version ${AZLW_CONTRACT_AGENT_VERSION}")
endif()

string(JSON AZLW_CONTRACT_MINIMUM_TIMEOUT_MS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants minimum_timeout_ms)
string(JSON AZLW_CONTRACT_MAXIMUM_TIMEOUT_MS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_timeout_ms)
string(JSON AZLW_CONTRACT_MAXIMUM_HANDSHAKE_ATTEMPTS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_handshake_attempts)
string(JSON AZLW_CONTRACT_UNAUTHENTICATED_PROBE_CONNECTIONS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants unauthenticated_probe_connections)
string(JSON AZLW_CONTRACT_MAXIMUM_REQUEST_BYTES
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_request_bytes)
string(JSON AZLW_CONTRACT_MAXIMUM_RESPONSE_BYTES
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_response_bytes)
string(JSON AZLW_CONTRACT_MAXIMUM_SNAPSHOT_ITEMS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_snapshot_items)
string(JSON AZLW_CONTRACT_MAXIMUM_EQUIPMENT_PAGE_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_equipment_page_size)
string(JSON AZLW_CONTRACT_MAXIMUM_EQUIPMENT_FRAME_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_equipment_frame_size)
string(JSON AZLW_CONTRACT_MAXIMUM_EQUIPMENT_CATALOG_ITEMS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_equipment_catalog_items)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_CATALOG_PAGE_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_catalog_page_size)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_CATALOG_FRAME_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_catalog_frame_size)
string(JSON AZLW_CONTRACT_MAXIMUM_DOCK_PAGE_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_dock_page_size)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_CATALOG_ITEMS
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_catalog_items)
string(JSON AZLW_CONTRACT_MAXIMUM_EQUIPMENT_WEAPON_BATCH_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_equipment_weapon_batch_size)
string(JSON AZLW_CONTRACT_MAXIMUM_SKILL_EFFECT_BATCH_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_skill_effect_batch_size)
string(JSON AZLW_CONTRACT_MAXIMUM_EQUIPMENT_REFERENCE_BATCH_SIZE
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_equipment_reference_batch_size)
string(JSON AZLW_CONTRACT_SHIP_EQUIPMENT_SLOT_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants ship_equipment_slot_count)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_SLOT_EQUIPMENT_TYPE_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_slot_equipment_type_count)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_SKILL_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_skill_count)
string(JSON AZLW_CONTRACT_MAXIMUM_FLEET_TEAM_SHIP_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_fleet_team_ship_count)
string(JSON AZLW_CONTRACT_MAXIMUM_SHIP_FLEET_MEMBERSHIP_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_ship_fleet_membership_count)
string(JSON AZLW_CONTRACT_MAXIMUM_ENHANCE_MATERIAL_COUNT
    GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" constants maximum_enhance_material_count)
string(JSON AZLW_OPERATION_COUNT
    LENGTH "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" operations)
if(AZLW_OPERATION_COUNT LESS 1)
    message(FATAL_ERROR "runtime RPC contract operations must not be empty")
endif()
math(EXPR AZLW_OPERATION_LAST "${AZLW_OPERATION_COUNT} - 1")
set(AZLW_OPERATION_CONSTANTS "")
set(AZLW_OPERATION_NAME_LIST "")
foreach(AZLW_OPERATION_INDEX RANGE 0 ${AZLW_OPERATION_LAST})
    string(JSON AZLW_OPERATION_NAME
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" operations ${AZLW_OPERATION_INDEX})
    string(REPLACE "_" ";" AZLW_OPERATION_PARTS "${AZLW_OPERATION_NAME}")
    set(AZLW_OPERATION_IDENT "")
    foreach(AZLW_OPERATION_PART ${AZLW_OPERATION_PARTS})
        string(SUBSTRING "${AZLW_OPERATION_PART}" 0 1 AZLW_OPERATION_FIRST)
        string(SUBSTRING "${AZLW_OPERATION_PART}" 1 -1 AZLW_OPERATION_REST)
        string(TOUPPER "${AZLW_OPERATION_FIRST}" AZLW_OPERATION_FIRST)
        string(APPEND AZLW_OPERATION_IDENT "${AZLW_OPERATION_FIRST}${AZLW_OPERATION_REST}")
    endforeach()
    string(APPEND AZLW_OPERATION_CONSTANTS
        "inline constexpr char kRpcOperation${AZLW_OPERATION_IDENT}[] = \"${AZLW_OPERATION_NAME}\";\n")
    string(APPEND AZLW_OPERATION_NAME_LIST
        "    kRpcOperation${AZLW_OPERATION_IDENT},\n")
endforeach()

string(JSON AZLW_SECURE_CHANNEL_SCHEMA_VERSION
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" schema_version)
if(NOT AZLW_SECURE_CHANNEL_SCHEMA_VERSION EQUAL 1)
    message(FATAL_ERROR
        "unsupported secure-channel contract schema version ${AZLW_SECURE_CHANNEL_SCHEMA_VERSION}")
endif()
string(JSON AZLW_SECURE_CHANNEL_VERSION
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants channel_version)
string(JSON AZLW_SECURE_SERVER_CHALLENGE_TYPE
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants server_challenge_type)
string(JSON AZLW_SECURE_CLIENT_PROOF_TYPE
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants client_proof_type)
string(JSON AZLW_SECURE_PROTECTED_FRAME_TYPE
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants protected_frame_type)
string(JSON AZLW_SECURE_SESSION_ID_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants session_id_bytes)
string(JSON AZLW_SECURE_SECRET_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants secret_bytes)
string(JSON AZLW_SECURE_HANDSHAKE_NONCE_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants handshake_nonce_bytes)
string(JSON AZLW_SECURE_PROOF_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants proof_bytes)
string(JSON AZLW_SECURE_KEY_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants key_bytes)
string(JSON AZLW_SECURE_NONCE_PREFIX_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants nonce_prefix_bytes)
string(JSON AZLW_SECURE_AEAD_NONCE_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants aead_nonce_bytes)
string(JSON AZLW_SECURE_AEAD_TAG_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants aead_tag_bytes)
string(JSON AZLW_SECURE_SERVER_CHALLENGE_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants server_challenge_bytes)
string(JSON AZLW_SECURE_CLIENT_PROOF_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants client_proof_bytes)
string(JSON AZLW_SECURE_PROTECTED_HEADER_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants protected_header_bytes)
string(JSON AZLW_SECURE_PROTECTED_OVERHEAD_BYTES
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" constants protected_overhead_bytes)
string(JSON AZLW_SECURE_SERVER_PROOF_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels server_proof)
string(JSON AZLW_SECURE_CLIENT_PROOF_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels client_proof)
string(JSON AZLW_SECURE_HOST_TO_AGENT_KEY_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels host_to_agent_key)
string(JSON AZLW_SECURE_AGENT_TO_HOST_KEY_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels agent_to_host_key)
string(JSON AZLW_SECURE_HOST_TO_AGENT_NONCE_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels host_to_agent_nonce)
string(JSON AZLW_SECURE_AGENT_TO_HOST_NONCE_LABEL
    GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" labels agent_to_host_nonce)
file(MAKE_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/generated")
configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/common/runtime_rpc_contract.h.in"
    "${CMAKE_CURRENT_BINARY_DIR}/generated/runtime_rpc_contract.h"
    @ONLY
)
configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/common/secure_channel_contract.h.in"
    "${CMAKE_CURRENT_BINARY_DIR}/generated/secure_channel_contract.h"
    @ONLY
)

add_compile_definitions(AZLW_AGENT_VERSION="${AZLW_CONTRACT_AGENT_VERSION}")

if(NOT ANDROID)
    message(FATAL_ERROR "azlw native runtime only supports the Android NDK toolchain")
endif()

if(NOT CMAKE_ANDROID_ARCH_ABI STREQUAL "x86_64")
    message(FATAL_ERROR "azlw native runtime only supports Android x86_64")
endif()

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

set(ANDKITTY_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/third_party/AndKittyInjector")
set(ANDKITTY_SOURCE_ROOT "${ANDKITTY_ROOT}/AndKittyInjector/src")
set(KITTYMEMORY_ROOT "${ANDKITTY_ROOT}/KittyMemoryEx/KittyMemoryEx")
set(JSMN_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/third_party/jsmn")
set(MONOCYPHER_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/third_party/Monocypher")

file(GLOB KITTYMEMORY_SOURCES CONFIGURE_DEPENDS "${KITTYMEMORY_ROOT}/*.cpp")

add_library(azlw-common STATIC common/bootstrap_validation.cpp)
target_include_directories(azlw-common PUBLIC
    "${CMAKE_CURRENT_SOURCE_DIR}/common"
    "${CMAKE_CURRENT_BINARY_DIR}/generated"
)
target_compile_options(azlw-common PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-common PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-monocypher STATIC "${MONOCYPHER_ROOT}/src/monocypher.c")
target_include_directories(azlw-monocypher PUBLIC "${MONOCYPHER_ROOT}/src")
target_compile_definitions(azlw-monocypher PRIVATE BLAKE2_NO_UNROLLING)
target_compile_options(azlw-monocypher PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-monocypher PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-secure-channel STATIC common/secure_channel.cpp)
target_include_directories(azlw-secure-channel PUBLIC
    "${CMAKE_CURRENT_SOURCE_DIR}/common"
    "${CMAKE_CURRENT_BINARY_DIR}/generated"
)
target_link_libraries(azlw-secure-channel PUBLIC azlw-monocypher)
target_compile_options(azlw-secure-channel PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-secure-channel PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-agent-protocol STATIC
    agent/protocol/handshake.cpp
    agent/protocol/json_codec.cpp
    agent/protocol/json_codec_common.cpp
    agent/protocol/json_command_parser.cpp
    agent/protocol/json_encoder.cpp
    agent/protocol/json_equipment_encoder.cpp
    agent/protocol/json_state_encoder.cpp
    agent/protocol/json_writer.cpp
)
target_include_directories(azlw-agent-protocol PUBLIC
    "${CMAKE_CURRENT_SOURCE_DIR}/agent"
    "${JSMN_ROOT}"
)
target_link_libraries(azlw-agent-protocol PUBLIC azlw-common azlw-secure-channel)
target_compile_options(azlw-agent-protocol PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-agent-protocol PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-agent-lua STATIC
    agent/snapshots/bag_snapshot.cpp
    agent/snapshots/bay_ship_reader.cpp
    agent/commands/equipment_command.cpp
    agent/commands/equipment_command_access.cpp
    agent/commands/equipment_command_observe.cpp
    agent/snapshots/equipment_config_snapshot.cpp
    agent/snapshots/equipment_effect_snapshot.cpp
    agent/snapshots/equipment_reference_snapshot.cpp
    agent/snapshots/fleet_membership_reader.cpp
    agent/lua/lua_api.cpp
    agent/lua/lua_reader.cpp
    agent/lua/lua_value.cpp
    agent/lua/module_view.cpp
    agent/snapshots/owned_state_snapshot.cpp
    agent/snapshots/ship_details_snapshot.cpp
    agent/snapshots/ship_catalog_snapshot.cpp
    agent/snapshots/ship_skill_reader.cpp
)
target_include_directories(azlw-agent-lua PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/agent")
target_link_libraries(azlw-agent-lua PUBLIC azlw-agent-protocol dl)
target_compile_options(azlw-agent-lua PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-agent-lua PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-agent-core STATIC
    agent/commands/equipment_command_ledger.cpp
    agent/runtime/hook_x86_64.cpp
    agent/runtime/rpc_server.cpp
    agent/runtime/runtime.cpp
    agent/runtime/task_queue.cpp
    agent/runtime/secure_random.cpp
)
target_include_directories(azlw-agent-core PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}/agent")
target_link_libraries(azlw-agent-core PUBLIC azlw-agent-lua azlw-agent-protocol azlw-common dl log)
target_compile_options(azlw-agent-core PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-agent-core PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_library(azlw-agent-x86_64 SHARED agent/agent_entry.cpp)
target_link_libraries(azlw-agent-x86_64 PRIVATE azlw-agent-core)
target_compile_options(azlw-agent-x86_64 PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -fvisibility=hidden
    -fvisibility-inlines-hidden
    -ffunction-sections
    -fdata-sections
)
target_link_options(azlw-agent-x86_64 PRIVATE
    -Wl,--gc-sections
    -Wl,--exclude-libs,ALL
    -Wl,-z,defs
    -Wl,--undefined=azlw_agent_version_marker
)
set_target_properties(azlw-agent-x86_64 PROPERTIES
    LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/artifacts"
    OUTPUT_NAME "azlw-agent-x86_64"
    NO_SONAME ON
    CXX_VISIBILITY_PRESET hidden
    VISIBILITY_INLINES_HIDDEN YES
)

if(AZLW_BUILD_TESTS)
    enable_testing()

    string(JSON AZLW_SECURE_FIXTURE_SESSION_ID_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture session_id_hex)
    string(JSON AZLW_SECURE_FIXTURE_SESSION_SECRET_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture session_secret_hex)
    string(JSON AZLW_SECURE_FIXTURE_SERVER_NONCE_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture server_nonce_hex)
    string(JSON AZLW_SECURE_FIXTURE_CLIENT_NONCE_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture client_nonce_hex)
    string(JSON AZLW_SECURE_FIXTURE_SERVER_CHALLENGE_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture server_challenge_hex)
    string(JSON AZLW_SECURE_FIXTURE_CLIENT_PROOF_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture client_proof_hex)
    string(JSON AZLW_SECURE_FIXTURE_HOST_TO_AGENT_KEY_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture host_to_agent_key_hex)
    string(JSON AZLW_SECURE_FIXTURE_AGENT_TO_HOST_KEY_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture agent_to_host_key_hex)
    string(JSON AZLW_SECURE_FIXTURE_HOST_TO_AGENT_NONCE_PREFIX_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture host_to_agent_nonce_prefix_hex)
    string(JSON AZLW_SECURE_FIXTURE_AGENT_TO_HOST_NONCE_PREFIX_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture agent_to_host_nonce_prefix_hex)
    string(JSON AZLW_SECURE_FIXTURE_PROTECTED_SEQUENCE
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture protected_sequence)
    string(JSON AZLW_SECURE_FIXTURE_PROTECTED_PLAINTEXT_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture protected_plaintext_hex)
    string(JSON AZLW_SECURE_FIXTURE_PROTECTED_FRAME_HEX
        GET "${AZLW_SECURE_CHANNEL_CONTRACT_JSON}" fixture protected_frame_hex)
    configure_file(
        "${CMAKE_CURRENT_SOURCE_DIR}/tests/common/secure_channel_contract_data.h.in"
        "${CMAKE_CURRENT_BINARY_DIR}/generated/secure_channel_contract_data.h"
        @ONLY
    )

    add_executable(azlw-secure-channel-test tests/common/secure_channel_test.cpp)
    target_include_directories(azlw-secure-channel-test PRIVATE
        "${CMAKE_CURRENT_BINARY_DIR}/generated"
    )
    target_link_libraries(azlw-secure-channel-test PRIVATE azlw-secure-channel)
    target_compile_options(azlw-secure-channel-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-secure-channel-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    string(JSON AZLW_CONTRACT_HANDSHAKE_RESPONSE
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" frames handshake_response)
    string(JSON AZLW_CONTRACT_SNAPSHOT_BAG_REQUEST
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" frames snapshot_bag_request)
    string(JSON AZLW_CONTRACT_UNKNOWN_OPERATION_REQUEST
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" frames unknown_operation_request)
    string(JSON AZLW_CONTRACT_HEALTH_RESPONSE
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" frames health_response)
    string(JSON AZLW_CONTRACT_ERROR_RESPONSE
        GET "${AZLW_RUNTIME_RPC_CONTRACT_JSON}" frames error_response)
    foreach(AZLW_CONTRACT_FRAME IN ITEMS
        AZLW_CONTRACT_HANDSHAKE_RESPONSE
        AZLW_CONTRACT_SNAPSHOT_BAG_REQUEST
        AZLW_CONTRACT_UNKNOWN_OPERATION_REQUEST
        AZLW_CONTRACT_HEALTH_RESPONSE
        AZLW_CONTRACT_ERROR_RESPONSE
    )
        string(FIND "${${AZLW_CONTRACT_FRAME}}" ")azlwv1fixture\"" AZLW_FRAME_DELIMITER_INDEX)
        if(NOT AZLW_FRAME_DELIMITER_INDEX EQUAL -1)
            message(FATAL_ERROR
                "runtime RPC contract frame ${AZLW_CONTRACT_FRAME} collides with the native raw-string delimiter")
        endif()
    endforeach()
    configure_file(
        "${CMAKE_CURRENT_SOURCE_DIR}/tests/agent/runtime_rpc_contract_data.h.in"
        "${CMAKE_CURRENT_BINARY_DIR}/generated/runtime_rpc_contract_data.h"
        @ONLY
    )

    add_executable(azlw-agent-protocol-test tests/agent/agent_protocol_test.cpp)
    target_include_directories(azlw-agent-protocol-test PRIVATE
        "${CMAKE_CURRENT_BINARY_DIR}/generated"
    )
    target_link_libraries(azlw-agent-protocol-test PRIVATE azlw-agent-protocol)
    target_compile_options(azlw-agent-protocol-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-agent-protocol-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-task-queue-test tests/agent/task_queue_test.cpp)
    target_link_libraries(azlw-task-queue-test PRIVATE azlw-agent-core)
    target_compile_options(azlw-task-queue-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-task-queue-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-agent-rpc-lifecycle-test tests/agent/agent_rpc_lifecycle_test.cpp)
    target_link_libraries(azlw-agent-rpc-lifecycle-test PRIVATE azlw-agent-core)
    target_compile_options(azlw-agent-rpc-lifecycle-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-agent-rpc-lifecycle-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-bag-snapshot-test tests/agent/bag_snapshot_test.cpp)
    target_link_libraries(azlw-bag-snapshot-test PRIVATE azlw-agent-lua)
    target_compile_options(azlw-bag-snapshot-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-bag-snapshot-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-equipment-command-test tests/agent/equipment_command_test.cpp)
    target_link_libraries(azlw-equipment-command-test PRIVATE azlw-agent-lua)
    target_compile_options(azlw-equipment-command-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-equipment-command-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-equipment-command-ledger-test tests/agent/equipment_command_ledger_test.cpp)
    target_link_libraries(azlw-equipment-command-ledger-test PRIVATE azlw-agent-core)
    target_compile_options(azlw-equipment-command-ledger-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-equipment-command-ledger-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(
        azlw-ship-catalog-snapshot-test
        tests/agent/ship_catalog_snapshot_test.cpp
    )
    target_link_libraries(azlw-ship-catalog-snapshot-test PRIVATE azlw-agent-lua)
    target_compile_options(azlw-ship-catalog-snapshot-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-ship-catalog-snapshot-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(
        azlw-bay-ship-reader-test
        tests/agent/bay_ship_reader_test.cpp
    )
    target_link_libraries(azlw-bay-ship-reader-test PRIVATE azlw-agent-lua)
    target_compile_options(azlw-bay-ship-reader-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-bay-ship-reader-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(
        azlw-equipment-batch-test
        tests/agent/equipment_batch_test.cpp
    )
    target_link_libraries(azlw-equipment-batch-test PRIVATE azlw-agent-lua)
    target_compile_options(azlw-equipment-batch-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
    )
    set_target_properties(azlw-equipment-batch-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(
        azlw-loader-policy-test
        tests/loader/loader_policy_test.cpp
        loader/process/remote_memory_evidence.cpp
    )
    target_include_directories(azlw-loader-policy-test PRIVATE
        "${CMAKE_CURRENT_BINARY_DIR}/generated"
        "${CMAKE_CURRENT_SOURCE_DIR}/common"
        "${CMAKE_CURRENT_SOURCE_DIR}/loader"
        "${ANDKITTY_SOURCE_ROOT}"
        "${KITTYMEMORY_ROOT}"
    )
    target_compile_definitions(azlw-loader-policy-test PRIVATE kNO_KEYSTONE)
    target_compile_options(azlw-loader-policy-test PRIVATE
        -Wall
        -Wextra
        -Wpedantic
        -Werror
        -Wno-gnu-statement-expression-from-macro-expansion
        -Wno-gnu-zero-variadic-macro-arguments
        -Wno-unused-command-line-argument
    )
    set_target_properties(azlw-loader-policy-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-module-identity-test
        tests/loader/module_identity_test.cpp
        loader/mapping/module_identity.cpp
    )
    target_include_directories(azlw-module-identity-test PRIVATE
        "${CMAKE_CURRENT_BINARY_DIR}/generated"
        "${CMAKE_CURRENT_SOURCE_DIR}/loader"
        "${CMAKE_CURRENT_SOURCE_DIR}/common"
    )
    target_link_libraries(azlw-module-identity-test PRIVATE azlw-andkitty dl log)
    target_compile_options(azlw-module-identity-test PRIVATE
        -Wall -Wextra -Wpedantic -Werror
        -Wno-gnu-statement-expression-from-macro-expansion
    )
    target_link_options(azlw-module-identity-test PRIVATE -Wl,--wrap=read)
    set_target_properties(azlw-module-identity-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    add_executable(azlw-thread-freeze-test
        tests/loader/thread_freeze_test.cpp
        loader/process/thread_freeze.cpp
        loader/process/remote_call_stack.cpp
        loader/process/tracee_ptrace.cpp
    )
    target_include_directories(azlw-thread-freeze-test PRIVATE
        "${CMAKE_CURRENT_SOURCE_DIR}/loader"
    )
    target_link_libraries(azlw-thread-freeze-test PRIVATE azlw-andkitty dl log)
    target_compile_definitions(azlw-thread-freeze-test PRIVATE AZLW_PTRACE_TESTING)
    target_link_options(azlw-thread-freeze-test PRIVATE -Wl,--wrap=ptrace)
    target_compile_options(azlw-thread-freeze-test PRIVATE
        -Wall -Wextra -Wpedantic -Werror
        -Wno-gnu-statement-expression-from-macro-expansion
        -Wno-gnu-zero-variadic-macro-arguments
    )
    set_target_properties(azlw-thread-freeze-test PROPERTIES
        RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/tests"
    )

    function(azlw_register_android_device_test target)
        add_test(
            NAME "${target}"
            COMMAND "$<TARGET_FILE:${target}>" ${ARGN}
        )
        set_tests_properties("${target}" PROPERTIES
            DISABLED TRUE
            LABELS "android-device"
            TIMEOUT 180
            WORKING_DIRECTORY "$<TARGET_FILE_DIR:${target}>"
        )
    endfunction()

    azlw_register_android_device_test(azlw-task-queue-test)
    azlw_register_android_device_test(azlw-agent-protocol-test)
    azlw_register_android_device_test(azlw-agent-rpc-lifecycle-test)
    azlw_register_android_device_test(azlw-bag-snapshot-test)
    azlw_register_android_device_test(azlw-equipment-command-test)
    azlw_register_android_device_test(azlw-equipment-command-ledger-test)
    azlw_register_android_device_test(azlw-loader-policy-test)
    azlw_register_android_device_test(azlw-module-identity-test)
    azlw_register_android_device_test(azlw-thread-freeze-test)
    azlw_register_android_device_test(azlw-ship-catalog-snapshot-test)
    azlw_register_android_device_test(azlw-bay-ship-reader-test)
    azlw_register_android_device_test(azlw-equipment-batch-test)
    azlw_register_android_device_test(azlw-secure-channel-test)
endif()

add_library(azlw-andkitty STATIC
    "${ANDKITTY_SOURCE_ROOT}/Injector/KittyInjector.cpp"
    ${KITTYMEMORY_SOURCES}
)

target_include_directories(azlw-andkitty PUBLIC
    "${ANDKITTY_SOURCE_ROOT}"
    "${KITTYMEMORY_ROOT}"
)
target_compile_definitions(azlw-andkitty PUBLIC
    kNO_KEYSTONE
    KITTY_INJECTOR_ENABLE_EMULATED=0
    KITTY_INJECTOR_ENABLE_FREE=0
    KITTY_INJECTOR_ENABLE_HIDE=0
    KITTY_INJECTOR_ENABLE_JNI_ENTRY=0
    KITTY_INJECTOR_ENABLE_BREAKPOINT=0
    KITTY_MEMORY_ENABLE_NATIVE_BRIDGE=0
    KITTY_MEMORY_ENABLE_PATCHING=0
    KITTY_MEMORY_ENABLE_MEM_IO=0
)
target_compile_options(azlw-andkitty PRIVATE
    -Wall
    -Wextra
    -Wno-gnu-statement-expression-from-macro-expansion
    -Wno-gnu-zero-variadic-macro-arguments
    -Wno-unused-command-line-argument
    -ffunction-sections
    -fdata-sections
)
set_target_properties(azlw-andkitty PROPERTIES
    CXX_EXTENSIONS ON
    INTERPROCEDURAL_OPTIMIZATION_RELEASE ON
)

add_executable(azlw-loader-x86_64
    loader/agent_unloader.cpp
    loader/load_transaction.cpp
    loader/mapping/anonymous_remap.cpp
    loader/mapping/anonymous_remap_kitty.cpp
    loader/session/bootstrap_file.cpp
    loader/mapping/elf_header_protection.cpp
    loader/mapping/elf_header_protection_kitty.cpp
    loader/main.cpp
    loader/mapping/module_identity.cpp
    loader/process/remote_call_stack.cpp
    loader/process/remote_memory_evidence.cpp
    loader/session/session_file.cpp
    loader/mapping/solist_visibility.cpp
    loader/mapping/solist_visibility_kitty.cpp
    loader/process/thread_freeze.cpp
    loader/process/tracee_ptrace.cpp
    loader/process/tracee_wait_status.cpp
    loader/session/unload_file.cpp
)

target_include_directories(azlw-loader-x86_64 PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/common"
    "${CMAKE_CURRENT_SOURCE_DIR}/loader"
)
target_compile_options(azlw-loader-x86_64 PRIVATE
    -Wall
    -Wextra
    -Wpedantic
    -Werror
    -Wno-gnu-statement-expression-from-macro-expansion
    -Wno-gnu-zero-variadic-macro-arguments
    -Wno-unused-command-line-argument
    -ffunction-sections
    -fdata-sections
)
target_link_options(azlw-loader-x86_64 PRIVATE
    -Wl,--gc-sections
    -Wl,--wrap=ptrace
    -Wl,--wrap=waitpid
)
target_link_libraries(azlw-loader-x86_64 PRIVATE azlw-common azlw-andkitty dl log)

set_target_properties(azlw-loader-x86_64 PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/artifacts"
    INTERPROCEDURAL_OPTIMIZATION_RELEASE ON
)

# 仅对 Release 目标分离调试信息，并在发布文件中保留可定位的 debuglink。
function(azlw_separate_debug_symbols target debug_name)
    if(NOT CMAKE_BUILD_TYPE STREQUAL "Release")
        return()
    endif()
    set(debug_file "${CMAKE_BINARY_DIR}/symbols/${debug_name}")
    add_custom_command(TARGET "${target}" POST_BUILD
        COMMAND "${CMAKE_COMMAND}" -E make_directory "${CMAKE_BINARY_DIR}/symbols"
        COMMAND "${CMAKE_OBJCOPY}" --only-keep-debug
            "$<TARGET_FILE:${target}>" "${debug_file}"
        COMMAND "${CMAKE_STRIP}" --strip-all "$<TARGET_FILE:${target}>"
        COMMAND "${CMAKE_OBJCOPY}" --add-gnu-debuglink=${debug_file}
            "$<TARGET_FILE:${target}>"
        COMMENT "Separating ${target} debug symbols from the release artifact"
        VERBATIM
    )
endfunction()

azlw_separate_debug_symbols(azlw-loader-x86_64 "loader.debug")
azlw_separate_debug_symbols(azlw-agent-x86_64 "agent.debug")
