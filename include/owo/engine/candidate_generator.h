#pragma once

#include "owo/engine/input_schema.h"
#include "owo/engine/lexicon.h"
#include "owo/engine/language_model.h"
#include "owo/engine/user_frequency.h"

#include <cstddef>
#include <cstdint>
#include <functional>
#include <string>
#include <string_view>
#include <vector>

namespace owo::engine {

struct Candidate {
    std::string text;
    std::vector<std::string> syllables;
    std::int64_t score{};
    InputMatchKind match_kind{InputMatchKind::exact};
    std::vector<std::string> source_segments;
    std::size_t consumed_input_bytes{};

    bool operator==(const Candidate&) const = default;
};

struct CandidateGenerationMetrics {
    std::uint64_t lexicon_lookup_us{};
    std::uint64_t sort_us{};
    std::uint64_t lexicon_lookup_count{};
};

class CandidateGenerator {
public:
    explicit CandidateGenerator(const Lexicon& lexicon,
                                const BigramModel* bigram = nullptr,
                                const UserFrequencyModel* user_frequency = nullptr)
        : lexicon_(lexicon), bigram_(bigram), user_frequency_(user_frequency) {}
    [[nodiscard]] std::vector<Candidate> generate(const ParseResult& parsed,
                                                  std::size_t limit = 32,
                                                  bool contextual_ranking = false,
                                                  std::string_view language_context = {},
                                                  CandidateGenerationMetrics* metrics = nullptr,
                                                  const std::function<bool()>& cancelled = {}) const;

private:
    const Lexicon& lexicon_;
    const BigramModel* bigram_{};
    const UserFrequencyModel* user_frequency_{};
};

}  // namespace owo::engine
