#include "owo/model/libime_bridge.h"

#include <libime/core/languagemodel.h>
#include <libime/core/lattice.h>

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

std::vector<std::size_t> utf8_boundaries(const std::string_view text) {
    std::vector<std::size_t> result{0};
    for (std::size_t offset = 1; offset < text.size(); ++offset) {
        if ((static_cast<unsigned char>(text[offset]) & 0xc0U) != 0x80U)
            result.push_back(offset);
    }
    result.push_back(text.size());
    return result;
}

struct TokenizationState {
    libime::State language_state{};
    float score{};
};

TokenizationState score_best_tokenization(
    const libime::LanguageModel& model, const libime::State& initial_state,
    const std::string_view text) {
    constexpr std::size_t kMaximumTokenCharacters = 4;
    constexpr std::size_t kTokenizationBeamWidth = 8;
    const auto boundaries = utf8_boundaries(text);
    if (text.empty() || boundaries.size() <= 1) return {initial_state, 0.0F};
    const auto character_count = boundaries.size() - 1;
    std::vector<std::vector<TokenizationState>> chart(character_count + 1);
    chart[0].push_back({initial_state, 0.0F});
    for (std::size_t begin = 0; begin < character_count; ++begin) {
        if (chart[begin].empty()) continue;
        const auto maximum_end =
            std::min(character_count, begin + kMaximumTokenCharacters);
        for (std::size_t end = begin + 1; end <= maximum_end; ++end) {
            const auto token = text.substr(boundaries[begin],
                                           boundaries[end] - boundaries[begin]);
            const auto index = model.index(token);
            // Unknown multi-character spans are never useful: the same unknown
            // cost can be represented by their individual characters while
            // retaining every real word boundary for the n-gram model.
            if (end > begin + 1 && model.isUnknown(index, token)) continue;
            const libime::WordNode word(token, index);
            for (const auto& previous : chart[begin]) {
                libime::State next_state{};
                const auto transition =
                    model.score(previous.language_state, word, next_state);
                chart[end].push_back(
                    {next_state, previous.score + transition});
            }
        }
        for (std::size_t end = begin + 1; end <= maximum_end; ++end) {
            auto& states = chart[end];
            if (states.size() <= kTokenizationBeamWidth) continue;
            std::partial_sort(states.begin(),
                              states.begin() + kTokenizationBeamWidth,
                              states.end(),
                              [](const auto& left, const auto& right) {
                                  return left.score > right.score;
                              });
            states.resize(kTokenizationBeamWidth);
        }
    }
    const auto& completed = chart.back();
    if (completed.empty()) return {initial_state, 0.0F};
    return *std::max_element(completed.begin(), completed.end(),
                             [](const auto& left, const auto& right) {
                                 return left.score < right.score;
                             });
}

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
            *score = score_best_tokenization(
                         model, model.beginState(), candidate_view).score;
        } else {
            const std::string_view context_view(context);
            const auto prefix = score_best_tokenization(
                model, model.beginState(), context_view);
            *score = score_best_tokenization(
                         model, prefix.language_state, candidate_view).score;
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
