#include "owo/tsf/language_context.h"

#include <string>

int main() {
    std::wstring context;
    owo::tsf::append_bounded_language_context(context, L"你好");
    owo::tsf::append_bounded_language_context(context, L"世界");
    if (context != L"你好世界") return 1;

    owo::tsf::append_bounded_language_context(context, L"12345678901234567890");
    if (context != L"5678901234567890" || context.size() != 16) return 2;

    context.assign({static_cast<wchar_t>(0xd83d), static_cast<wchar_t>(0xde00)});
    context.append(15, L'x');
    owo::tsf::append_bounded_language_context(context, L"");
    if (context.size() != 15 || context.front() != L'x') return 3;

    owo::tsf::append_bounded_language_context(context, L"ignored", 0);
    if (!context.empty()) return 4;
    return 0;
}
