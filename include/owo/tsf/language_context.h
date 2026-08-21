#pragma once

#include <cstddef>
#include <string>
#include <string_view>

namespace owo::tsf {

inline void append_bounded_language_context(std::wstring& context,
                                            const std::wstring_view committed,
                                            const std::size_t maximum_utf16_units = 16) {
    if (maximum_utf16_units == 0) {
        context.clear();
        return;
    }
    context.append(committed);
    if (context.size() <= maximum_utf16_units) return;
    auto begin = context.size() - maximum_utf16_units;
    if (begin < context.size() && begin > 0 &&
        context[begin] >= 0xdc00 && context[begin] <= 0xdfff &&
        context[begin - 1] >= 0xd800 && context[begin - 1] <= 0xdbff)
        ++begin;
    context.erase(0, begin);
}

}  // namespace owo::tsf
