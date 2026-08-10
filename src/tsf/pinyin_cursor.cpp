#include "owo/tsf/pinyin_cursor.h"

#include <algorithm>

namespace owo::tsf {
namespace {

bool consumes_input(const wchar_t preview_character,
                    const wchar_t input_character) noexcept {
    if (preview_character == input_character) return true;
    // The case-preserving preview normally makes these identical. Treat any
    // non-separator preview character as the corresponding raw letter so a
    // temporarily stale parser response cannot strand the caret.
    return preview_character != L'\'' && input_character != L'\'';
}

}  // namespace

std::size_t preview_index_for_input_cursor(
    const std::wstring_view input, const std::wstring_view preview,
    const std::size_t input_cursor) noexcept {
    const auto target = std::min(input_cursor, input.size());
    std::size_t raw = 0;
    for (std::size_t shown = 0; shown < preview.size(); ++shown) {
        if (raw >= target) return shown;
        if (raw < input.size() && consumes_input(preview[shown], input[raw])) ++raw;
    }
    return preview.size();
}

std::size_t input_cursor_for_preview_index(
    const std::wstring_view input, const std::wstring_view preview,
    const std::size_t preview_index) noexcept {
    const auto target = std::min(preview_index, preview.size());
    std::size_t raw = 0;
    for (std::size_t shown = 0; shown < target && raw < input.size(); ++shown) {
        if (consumes_input(preview[shown], input[raw])) ++raw;
    }
    return raw;
}

std::size_t preview_character_for_input_index(
    const std::wstring_view input, const std::wstring_view preview,
    const std::size_t input_index) noexcept {
    if (input_index >= input.size()) return preview.size();
    std::size_t raw = 0;
    for (std::size_t shown = 0; shown < preview.size(); ++shown) {
        if (raw == input_index && consumes_input(preview[shown], input[raw])) return shown;
        if (raw < input.size() && consumes_input(preview[shown], input[raw])) ++raw;
    }
    return preview.size();
}

void insert_at_pinyin_cursor(std::wstring& input, std::wstring& preview,
                             std::size_t& input_cursor,
                             const wchar_t character) {
    input_cursor = std::min(input_cursor, input.size());
    const auto shown_cursor = preview.empty()
                                  ? input_cursor
                                  : preview_index_for_input_cursor(
                                        input, preview, input_cursor);
    input.insert(input.begin() + static_cast<std::ptrdiff_t>(input_cursor),
                 character);
    if (!preview.empty())
        preview.insert(preview.begin() + static_cast<std::ptrdiff_t>(shown_cursor),
                       character);
    ++input_cursor;
}

bool erase_before_pinyin_cursor(std::wstring& input, std::wstring& preview,
                                std::size_t& input_cursor) {
    input_cursor = std::min(input_cursor, input.size());
    if (input_cursor == 0) return false;
    const auto erased_input = input_cursor - 1;
    if (!preview.empty()) {
        const auto shown = preview_character_for_input_index(
            input, preview, erased_input);
        if (shown < preview.size()) preview.erase(shown, 1);
    }
    input.erase(erased_input, 1);
    --input_cursor;
    return true;
}

}  // namespace owo::tsf
