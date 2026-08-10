#include "owo/model/libime_bridge.h"

#include <libime/core/languagemodel.h>

#include <algorithm>
#include <cstring>
#include <exception>
#include <memory>
#include <string_view>
#include <vector>

namespace {

struct BridgeState {
    explicit BridgeState(const char* path) : model(path) {}
    libime::LanguageModel model;
};

void set_diagnostic(char* output, const size_t output_size, const std::string_view value) {
    if (output == nullptr || output_size == 0) return;
    const auto count = (std::min)(output_size - 1, value.size());
    std::memcpy(output, value.data(), count);
    output[count] = '\0';
}

}  // namespace

extern "C" uint32_t owo_libime_abi_version(void) { return OWO_LIBIME_BRIDGE_ABI_VERSION; }

extern "C" owo_libime_handle owo_libime_open(const char* model_path,
                                               char* diagnostic,
                                               const size_t diagnostic_size) {
    if (model_path == nullptr || model_path[0] == '\0') {
        set_diagnostic(diagnostic, diagnostic_size, "model path is empty");
        return nullptr;
    }
    try {
        auto state = std::make_unique<BridgeState>(model_path);
        set_diagnostic(diagnostic, diagnostic_size, {});
        return state.release();
    } catch (const std::exception& error) {
        set_diagnostic(diagnostic, diagnostic_size, error.what());
    } catch (...) {
        set_diagnostic(diagnostic, diagnostic_size, "unknown libime load failure");
    }
    return nullptr;
}

extern "C" int owo_libime_score(const owo_libime_handle handle,
                                 const char* context,
                                 const char* candidate,
                                 float* score,
                                 char* diagnostic,
                                 const size_t diagnostic_size) {
    if (handle == nullptr || candidate == nullptr || candidate[0] == '\0' || score == nullptr) {
        set_diagnostic(diagnostic, diagnostic_size, "invalid score request");
        return 0;
    }
    try {
        const auto& model = static_cast<const BridgeState*>(handle)->model;
        const std::string_view candidate_view(candidate);
        if (context == nullptr || context[0] == '\0') {
            *score = model.singleWordScore(model.beginState(), candidate_view);
        } else {
            const std::string_view context_view(context);
            const std::vector<std::string_view> prefix{context_view};
            const std::vector<std::string_view> sequence{context_view, candidate_view};
            *score = model.wordsScore(model.beginState(), sequence) -
                     model.wordsScore(model.beginState(), prefix);
        }
        set_diagnostic(diagnostic, diagnostic_size, {});
        return 1;
    } catch (const std::exception& error) {
        set_diagnostic(diagnostic, diagnostic_size, error.what());
    } catch (...) {
        set_diagnostic(diagnostic, diagnostic_size, "unknown libime scoring failure");
    }
    return 0;
}

extern "C" int owo_libime_score_batch(const owo_libime_handle handle,
                                        const char* context,
                                        const char* const* candidates,
                                        const std::size_t candidate_count,
                                        float* scores,
                                        char* diagnostic,
                                        const std::size_t diagnostic_size) {
    if (handle == nullptr || candidates == nullptr || candidate_count == 0 ||
        scores == nullptr) {
        set_diagnostic(diagnostic, diagnostic_size, "invalid batch score request");
        return 0;
    }
    for (std::size_t index = 0; index < candidate_count; ++index) {
        if (candidates[index] == nullptr || candidates[index][0] == '\0' ||
            !owo_libime_score(handle, context, candidates[index], &scores[index],
                              diagnostic, diagnostic_size))
            return 0;
    }
    set_diagnostic(diagnostic, diagnostic_size, {});
    return 1;
}

extern "C" void owo_libime_close(const owo_libime_handle handle) {
    delete static_cast<BridgeState*>(handle);
}
