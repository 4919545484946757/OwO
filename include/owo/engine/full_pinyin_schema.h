#pragma once

#include "owo/engine/input_schema.h"

#include <functional>

namespace owo::engine {

struct FullPinyinParseMetrics {
    std::uint64_t normalization_us{};
    std::uint64_t segmentation_us{};
    std::uint64_t correction_us{};
};

struct FullPinyinIncrementalState {
    std::string input;
    ParseResult result;
    std::size_t max_paths{};
};

class FullPinyinSchema final : public InputSchema {
public:
    FullPinyinSchema();
    [[nodiscard]] ParseResult parse(std::string_view input,
                                    std::size_t max_paths = 32) const override;
    [[nodiscard]] ParseResult parse(std::string_view input, std::size_t max_paths,
                                    bool correction_enabled) const;
    [[nodiscard]] ParseResult parse(std::string_view input, std::size_t max_paths,
                                    bool correction_enabled,
                                    FullPinyinParseMetrics* metrics,
                                    const std::function<bool()>& cancelled = {}) const;
    [[nodiscard]] ParseResult parse_incremental(
        std::string_view input, std::size_t max_paths,
        FullPinyinIncrementalState& state,
        FullPinyinParseMetrics* metrics = nullptr,
        const std::function<bool()>& cancelled = {},
        bool* reused = nullptr) const;
};

}  // namespace owo::engine
