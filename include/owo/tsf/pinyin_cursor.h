#pragma once

#include <cstddef>
#include <string>
#include <string_view>

namespace owo::tsf {

// Converts between the raw input buffer and the rendered preview, whose
// apostrophes may have been inserted automatically by the parser.
[[nodiscard]] std::size_t preview_index_for_input_cursor(
    std::wstring_view input, std::wstring_view preview, std::size_t input_cursor) noexcept;
[[nodiscard]] std::size_t input_cursor_for_preview_index(
    std::wstring_view input, std::wstring_view preview, std::size_t preview_index) noexcept;
[[nodiscard]] std::size_t preview_character_for_input_index(
    std::wstring_view input, std::wstring_view preview, std::size_t input_index) noexcept;
void insert_at_pinyin_cursor(std::wstring& input, std::wstring& preview,
                             std::size_t& input_cursor, wchar_t character);
[[nodiscard]] bool erase_before_pinyin_cursor(std::wstring& input,
                                               std::wstring& preview,
                                               std::size_t& input_cursor);

}  // namespace owo::tsf
