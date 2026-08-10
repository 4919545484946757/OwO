#include "owo/model/model_backend.h"

#include "owo/model/libime_bridge.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <numeric>
#include <utility>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#endif

namespace owo::model {
namespace {

#ifdef _WIN32

using AbiVersionFunction = std::uint32_t (*)();
using OpenFunction = owo_libime_handle (*)(const char*, char*, std::size_t);
using ScoreFunction = int (*)(owo_libime_handle, const char*, const char*, float*, char*,
                              std::size_t);
using ScoreBatchFunction = int (*)(owo_libime_handle, const char*, const char* const*,
                                   std::size_t, float*, char*, std::size_t);
using CloseFunction = void (*)(owo_libime_handle);

class LibimeBackend final : public IModelBackend {
public:
    LibimeBackend(HMODULE module, owo_libime_handle handle, ScoreFunction score,
                  ScoreBatchFunction score_batch, CloseFunction close)
        : module_(module), handle_(handle), score_(score),
          score_batch_(score_batch), close_(close) {}

    ~LibimeBackend() override {
        if (handle_ != nullptr) close_(handle_);
        if (module_ != nullptr) FreeLibrary(module_);
    }

    [[nodiscard]] std::string_view id() const noexcept override {
        return "owo.libime.ngram.v1";
    }

    [[nodiscard]] ModelResult rank(const ModelRequest& request,
                                   const std::stop_token stop) override {
        if (request.candidates.empty())
            return {request.request_id, ModelStatus::backend_error, {}, "candidate list is empty"};
        if (request.timeout.count() <= 0)
            return {request.request_id, ModelStatus::timeout, {}, "deadline exceeded"};

        const auto deadline = std::chrono::steady_clock::now() + request.timeout;
        std::vector<float> scores(request.candidates.size());
        std::array<char, 512> diagnostic{};
        if (score_batch_ != nullptr) {
            std::vector<const char*> candidate_views;
            candidate_views.reserve(request.candidates.size());
            for (const auto& candidate : request.candidates)
                candidate_views.push_back(candidate.c_str());
            if (stop.stop_requested())
                return {request.request_id, ModelStatus::cancelled, {}, "cancelled"};
            if (!score_batch_(handle_, request.context.c_str(), candidate_views.data(),
                              candidate_views.size(), scores.data(), diagnostic.data(),
                              diagnostic.size()))
                return {request.request_id, ModelStatus::backend_error, {}, diagnostic.data()};
        } else {
            for (std::size_t index = 0; index < request.candidates.size(); ++index) {
                if (stop.stop_requested())
                    return {request.request_id, ModelStatus::cancelled, {}, "cancelled"};
                if (std::chrono::steady_clock::now() >= deadline)
                    return {request.request_id, ModelStatus::timeout, {}, "deadline exceeded"};
                if (!score_(handle_, request.context.c_str(), request.candidates[index].c_str(),
                            &scores[index], diagnostic.data(), diagnostic.size()))
                    return {request.request_id, ModelStatus::backend_error, {}, diagnostic.data()};
            }
        }
        if (!std::all_of(scores.begin(), scores.end(),
                         [](const float score) { return std::isfinite(score); }))
            return {request.request_id, ModelStatus::backend_error, {},
                    "libime returned a non-finite score"};

        std::vector<std::size_t> order(request.candidates.size());
        std::iota(order.begin(), order.end(), 0);
        std::stable_sort(order.begin(), order.end(),
                         [&](const auto left, const auto right) {
                             return scores[left] > scores[right];
                         });
        std::vector<std::string> ranked;
        ranked.reserve(order.size());
        for (const auto index : order) ranked.push_back(request.candidates[index]);
        return {request.request_id, ModelStatus::success, std::move(ranked), {}};
    }

private:
    HMODULE module_{};
    owo_libime_handle handle_{};
    ScoreFunction score_{};
    ScoreBatchFunction score_batch_{};
    CloseFunction close_{};
};

std::wstring utf8_to_wide(const std::string_view value) {
    if (value.empty()) return {};
    const auto size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
    if (size <= 0) return {};
    std::wstring result(static_cast<std::size_t>(size), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), result.data(), size) != size)
        return {};
    return result;
}

std::wstring absolute_path(const std::wstring& value) {
    const auto size = GetFullPathNameW(value.c_str(), 0, nullptr, nullptr);
    if (size == 0) return {};
    std::wstring result(static_cast<std::size_t>(size), L'\0');
    const auto written = GetFullPathNameW(value.c_str(), size, result.data(), nullptr);
    if (written == 0 || written >= size) return {};
    result.resize(written);
    return result;
}

#endif

}  // namespace

LibimeBackendLoadResult load_libime_backend(const std::string_view bridge_path,
                                            const std::string_view model_path) {
#ifdef _WIN32
    const auto wide_path = absolute_path(utf8_to_wide(bridge_path));
    if (wide_path.empty()) return {{}, "libime bridge path is empty or invalid UTF-8"};
    const auto module = LoadLibraryExW(wide_path.c_str(), nullptr,
                                       LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR |
                                           LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (module == nullptr) return {{}, "unable to load libime bridge"};

    const auto abi = reinterpret_cast<AbiVersionFunction>(
        GetProcAddress(module, "owo_libime_abi_version"));
    const auto open = reinterpret_cast<OpenFunction>(GetProcAddress(module, "owo_libime_open"));
    const auto score = reinterpret_cast<ScoreFunction>(GetProcAddress(module, "owo_libime_score"));
    const auto score_batch = reinterpret_cast<ScoreBatchFunction>(
        GetProcAddress(module, "owo_libime_score_batch"));
    const auto close = reinterpret_cast<CloseFunction>(GetProcAddress(module, "owo_libime_close"));
    if (abi == nullptr || open == nullptr || score == nullptr || close == nullptr ||
        abi() != OWO_LIBIME_BRIDGE_ABI_VERSION) {
        FreeLibrary(module);
        return {{}, "libime bridge ABI mismatch"};
    }

    std::array<char, 512> diagnostic{};
    const std::string model(model_path);
    const auto handle = open(model.c_str(), diagnostic.data(), diagnostic.size());
    if (handle == nullptr) {
        FreeLibrary(module);
        return {{}, diagnostic[0] == '\0' ? "unable to open libime model" : diagnostic.data()};
    }
    return {std::make_unique<LibimeBackend>(module, handle, score, score_batch, close), {}};
#else
    (void)bridge_path;
    (void)model_path;
    return {{}, "libime backend is supported on Windows only"};
#endif
}

}  // namespace owo::model
