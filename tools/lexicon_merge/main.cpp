#include "owo/engine/binary_lexicon.h"

#include <iostream>
#include <string>
#include <unordered_set>
#include <vector>

int main(int argc, char** argv) {
    if (argc < 4) {
        std::cerr << "usage: owo_lexicon_merge <output.owolx> <input.owolx> <input.owolx> [...]\n";
        return 2;
    }
    std::unordered_set<std::string> seen;
    std::vector<owo::engine::LexiconEntry> merged;
    for (int argument = 2; argument < argc; ++argument) {
        owo::engine::BinaryLexicon lexicon;
        const auto loaded = lexicon.load(argv[argument]);
        if (!loaded.success) {
            std::cerr << argv[argument] << ": " << loaded.error << '\n';
            return 3;
        }
        merged.reserve(merged.size() + lexicon.size());
        for (const auto& entry : lexicon.materialize_entries()) {
            std::string key;
            for (const auto& syllable : entry.syllables) key += syllable + '\0';
            key += entry.text;
            if (seen.insert(std::move(key)).second) merged.push_back(entry);
        }
    }
    const auto written = owo::engine::write_binary_lexicon(argv[1], std::move(merged));
    if (!written.success) {
        std::cerr << written.error << '\n';
        return 4;
    }
    return 0;
}
