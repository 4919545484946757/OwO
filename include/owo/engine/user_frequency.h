#pragma once

#include <cstdint>
#include <filesystem>
#include <string>
#include <string_view>
#include <unordered_map>

namespace owo::engine {

class UserFrequencyModel {
public:
    virtual ~UserFrequencyModel() = default;
    [[nodiscard]] virtual std::int64_t score(std::string_view text) const = 0;
    [[nodiscard]] virtual std::int64_t contextual_score(std::string_view input,
                                                        std::string_view text) const {
        (void)input;
        (void)text;
        return 0;
    }
    [[nodiscard]] virtual std::int64_t language_context_score(
        std::string_view context, std::string_view input, std::string_view text) const {
        (void)context;
        (void)input;
        (void)text;
        return 0;
    }
};

struct UserFrequencyIoResult {
    bool success{};
    bool recovered_from_backup{};
    std::string error;
};

class UserFrequencyStore final : public UserFrequencyModel {
public:
    [[nodiscard]] UserFrequencyIoResult load(const std::filesystem::path& path);
    void record(std::string_view text, std::uint32_t amount = 1);
    void record(std::string_view input, std::string_view text, std::uint32_t amount = 1);
    void record(std::string_view context, std::string_view input, std::string_view text,
                std::uint32_t amount = 1);
    void set_sensitivity(std::uint32_t sensitivity) noexcept;
    [[nodiscard]] std::uint32_t count(std::string_view text) const;
    [[nodiscard]] std::uint32_t contextual_count(std::string_view input,
                                                 std::string_view text) const;
    [[nodiscard]] std::int64_t score(std::string_view text) const override;
    [[nodiscard]] std::int64_t contextual_score(std::string_view input,
                                                std::string_view text) const override;
    [[nodiscard]] std::int64_t language_context_score(
        std::string_view context, std::string_view input,
        std::string_view text) const override;
    [[nodiscard]] UserFrequencyIoResult flush() const;

private:
    std::filesystem::path path_;
    std::unordered_map<std::string, std::uint32_t> counts_;
    std::unordered_map<std::string, std::uint32_t> contextual_counts_;
    std::unordered_map<std::string, std::uint32_t> language_context_counts_;
    std::uint32_t sensitivity_{7};
};

}  // namespace owo::engine
