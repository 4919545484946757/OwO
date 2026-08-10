#include "owo/tsf/pinyin_cursor.h"

#include <cstddef>
#include <iostream>
#include <string>
#include <string_view>

namespace {

bool expect(const std::size_t actual, const std::size_t expected,
            const std::string_view label) {
    if (actual == expected) return true;
    std::cerr << label << ": expected " << expected << ", got " << actual << '\n';
    return false;
}

}  // namespace

int main() {
    using owo::tsf::input_cursor_for_preview_index;
    using owo::tsf::preview_character_for_input_index;
    using owo::tsf::preview_index_for_input_cursor;

    constexpr std::wstring_view raw = L"nihaoshijie";
    constexpr std::wstring_view preview = L"ni'hao'shi'jie";
    if (!expect(preview_index_for_input_cursor(raw, preview, 0), 0, "start") ||
        !expect(preview_index_for_input_cursor(raw, preview, 2), 2, "before auto separator") ||
        !expect(preview_index_for_input_cursor(raw, preview, 5), 6, "middle") ||
        !expect(preview_index_for_input_cursor(raw, preview, raw.size()), preview.size(), "end") ||
        !expect(input_cursor_for_preview_index(raw, preview, 3), 2, "after auto separator") ||
        !expect(input_cursor_for_preview_index(raw, preview, 9), 7, "clicked middle") ||
        !expect(preview_character_for_input_index(raw, preview, 2), 3, "letter after separator"))
        return 1;

    constexpr std::wstring_view explicit_raw = L"ni'hao";
    constexpr std::wstring_view explicit_preview = L"ni'hao";
    if (!expect(preview_index_for_input_cursor(explicit_raw, explicit_preview, 3), 3,
                "explicit separator cursor") ||
        !expect(input_cursor_for_preview_index(explicit_raw, explicit_preview, 3), 3,
                "explicit separator click") ||
        !expect(preview_character_for_input_index(explicit_raw, explicit_preview, 2), 2,
                "explicit separator character"))
        return 2;

    std::wstring editable_input = L"nihao";
    std::wstring editable_preview = L"ni'hao";
    std::size_t editable_cursor = 2;
    owo::tsf::insert_at_pinyin_cursor(
        editable_input, editable_preview, editable_cursor, L'm');
    if (editable_input != L"nimhao" || editable_preview != L"nim'hao" ||
        editable_cursor != 3)
        return 3;
    if (!owo::tsf::erase_before_pinyin_cursor(
            editable_input, editable_preview, editable_cursor) ||
        editable_input != L"nihao" || editable_preview != L"ni'hao" ||
        editable_cursor != 2)
        return 4;
    return 0;
}
