#include "owo/ipc/named_pipe.h"
#include "owo/model/model_backend.h"
#include "owo/model/model_assets.h"
#include "owo/model/model_inference.h"
#ifdef OWO_HAS_ONNXRUNTIME
#include "owo/model/onnxruntime_session.h"
#endif

#include <charconv>
#include <chrono>
#include <Windows.h>
#include <iostream>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace {
std::string wide_to_utf8(const std::wstring_view value) {
    if (value.empty()) return {};
    const auto size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0,
                                          nullptr, nullptr);
    if (size <= 0) return {};
    std::string result(static_cast<std::size_t>(size), '\0');
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), result.data(), size,
                            nullptr, nullptr) != size)
        return {};
    return result;
}
}  // namespace

int wmain(int argc, wchar_t** wide_argv) {
    const auto process_started = std::chrono::steady_clock::now();
    std::vector<std::string> arguments;
    arguments.reserve(static_cast<std::size_t>(argc));
    for (int index = 0; index < argc; ++index) {
        auto converted = wide_to_utf8(wide_argv[index]);
        if (converted.empty() && wide_argv[index][0] != L'\0') return 2;
        arguments.push_back(std::move(converted));
    }

    owo::model::MockBackendOptions options;
    std::string asset_manifest;
    bool synthetic_session = false;
    bool onnxruntime_session = false;
    std::string libime_bridge;
    std::string libime_model;
    for (int index = 1; index < argc; ++index) {
        const std::string_view argument(arguments[index]);
        if (argument == "--fail") {
            options.fail = true;
        } else if (argument == "--latency-ms" && index + 1 < argc) {
            const std::string_view value(arguments[++index]);
            std::uint64_t milliseconds{};
            const auto parsed = std::from_chars(value.data(), value.data() + value.size(), milliseconds);
            if (parsed.ec != std::errc{} || parsed.ptr != value.data() + value.size() ||
                milliseconds > 60'000) return 2;
            options.latency = std::chrono::milliseconds(milliseconds);
        } else if (argument == "--asset-manifest" && index + 1 < argc) {
            asset_manifest = arguments[++index];
        } else if (argument == "--synthetic-session") {
            synthetic_session = true;
        } else if (argument == "--onnxruntime-session") {
            onnxruntime_session = true;
        } else if (argument == "--libime-bridge" && index + 1 < argc) {
            libime_bridge = arguments[++index];
        } else if (argument == "--libime-model" && index + 1 < argc) {
            libime_model = arguments[++index];
        } else {
            return 2;
        }
    }
    if ((synthetic_session && onnxruntime_session) ||
        ((synthetic_session || onnxruntime_session) && asset_manifest.empty())) return 2;
    if (libime_bridge.empty() != libime_model.empty() ||
        (!libime_bridge.empty() && (!asset_manifest.empty() || synthetic_session ||
                                    onnxruntime_session))) return 2;
    std::unique_ptr<owo::model::IModelBackend> backend;
    if (!libime_bridge.empty()) {
        const auto phase_started = std::chrono::steady_clock::now();
        auto loaded = owo::model::load_libime_backend(libime_bridge, libime_model);
        if (!loaded) {
            std::cerr << "libime backend creation failed: " << loaded.diagnostic << '\n';
            return 5;
        }
        backend = std::move(loaded.backend);
        const auto duration = std::chrono::duration_cast<std::chrono::microseconds>(
            std::chrono::steady_clock::now() - phase_started).count();
        std::clog << R"({"process":"model_host","module":"startup","level":"info","event_id":"libime_loaded","duration_us":)"
                  << duration << "}\n";
    }
    if (!asset_manifest.empty()) {
        auto loaded = owo::model::load_model_assets(asset_manifest);
        if (!loaded.ok) {
            std::cerr << "model asset validation failed: " << loaded.diagnostic << '\n';
            return 3;
        }
        if (onnxruntime_session) {
#ifdef OWO_HAS_ONNXRUNTIME
            const auto created = owo::model::create_onnxruntime_cpu_session(
                loaded.value.manifest, loaded.value.model_path);
            if (!created) {
                std::cerr << "ONNX Runtime session creation failed: " << created.diagnostic << '\n';
                return 4;
            }
            backend = std::make_unique<owo::model::AssetCandidateRanker>(
                std::move(loaded.value.manifest), std::move(loaded.value.vocabulary),
                created.session);
            std::cerr << "model assets validated; ONNX Runtime CPU session enabled\n";
#else
            std::cerr << "ONNX Runtime support is not compiled in\n";
            return 4;
#endif
        } else if (synthetic_session) {
            auto session = std::make_shared<owo::model::SyntheticInferenceSession>();
            backend = std::make_unique<owo::model::AssetCandidateRanker>(
                std::move(loaded.value.manifest), std::move(loaded.value.vocabulary),
                std::move(session));
            std::cerr << "model assets validated; synthetic inference session enabled\n";
        } else {
            std::cerr << "model assets validated; inference backend remains mock\n";
        }
    }
    if (!backend) backend = std::make_unique<owo::model::MockModelBackend>(options);
    const auto startup_duration = std::chrono::duration_cast<std::chrono::microseconds>(
        std::chrono::steady_clock::now() - process_started).count();
    std::clog << R"({"process":"model_host","module":"startup","level":"info","event_id":"model_initialized","duration_us":)"
              << startup_duration << "}\n";
    return owo::ipc::run_model_server(owo::ipc::kModelHostPipeName, *backend);
}
