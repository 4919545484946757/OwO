#include "owo/ipc/named_pipe.h"
#include "owo/ipc/contextual_candidate_order.h"
#include "owo/config/config_monitor.h"
#include "owo/model/model_backend.h"
#include "owo/model/model_protocol.h"

#include "owo/engine/candidate_generator.h"
#include "owo/engine/full_pinyin_schema.h"
#include "owo/engine/lexicon.h"
#include "owo/engine/user_frequency.h"

#include "owo/protocol/messages.h"

#include <Windows.h>
#include <sddl.h>

#include <algorithm>
#include <array>
#include <cstddef>
#include <condition_variable>
#include <cstdint>
#include <chrono>
#include <functional>
#include <future>
#include <iostream>
#include <list>
#include <limits>
#include <mutex>
#include <numeric>
#include <optional>
#include <string_view>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

namespace owo::ipc {
namespace {

class CurrentUserPipeSecurity final {
public:
    CurrentUserPipeSecurity() {
        HANDLE token = nullptr;
        if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) return;
        DWORD bytes = 0;
        GetTokenInformation(token, TokenUser, nullptr, 0, &bytes);
        std::vector<std::byte> storage(bytes);
        if (bytes == 0 || !GetTokenInformation(token, TokenUser, storage.data(), bytes, &bytes)) {
            CloseHandle(token);
            return;
        }
        const auto* user = reinterpret_cast<const TOKEN_USER*>(storage.data());
        LPWSTR sid_text = nullptr;
        if (!ConvertSidToStringSidW(user->User.Sid, &sid_text)) {
            CloseHandle(token);
            return;
        }
        CloseHandle(token);

        // Restrict the pipe to this Windows user, SYSTEM and administrators.
        // The low mandatory label is intentional: Core may be launched once by
        // the elevated installer, while TSF clients normally run at medium or
        // low integrity in browsers and other sandboxed text hosts.
        const std::wstring sddl = L"D:P(A;;GA;;;" + std::wstring(sid_text) +
            L")(A;;GA;;;SY)(A;;GA;;;BA)S:(ML;;NW;;;LW)";
        LocalFree(sid_text);
        if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.c_str(), SDDL_REVISION_1, &descriptor_, nullptr)) return;
        attributes_.nLength = sizeof(attributes_);
        attributes_.lpSecurityDescriptor = descriptor_;
        attributes_.bInheritHandle = FALSE;
    }

    CurrentUserPipeSecurity(const CurrentUserPipeSecurity&) = delete;
    CurrentUserPipeSecurity& operator=(const CurrentUserPipeSecurity&) = delete;
    ~CurrentUserPipeSecurity() {
        if (descriptor_ != nullptr) LocalFree(descriptor_);
    }

    [[nodiscard]] SECURITY_ATTRIBUTES* get() noexcept {
        return descriptor_ == nullptr ? nullptr : &attributes_;
    }
    [[nodiscard]] explicit operator bool() const noexcept { return descriptor_ != nullptr; }

private:
    PSECURITY_DESCRIPTOR descriptor_{};
    SECURITY_ATTRIBUTES attributes_{};
};

protocol::ValidationResult io_error(const char* operation) {
    const DWORD error = GetLastError();
    const auto code = error == ERROR_SEM_TIMEOUT || error == ERROR_TIMEOUT
                          ? protocol::ErrorCode::timeout
                          : protocol::ErrorCode::transport_unavailable;
    return {code,
            std::string(operation) + " failed with Win32 error " +
                std::to_string(error)};
}

using Deadline = std::chrono::steady_clock::time_point;

DWORD remaining_milliseconds(const Deadline deadline) {
    const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
        deadline - std::chrono::steady_clock::now());
    if (remaining.count() <= 0) return 0;
    return static_cast<DWORD>((std::min)(remaining.count(),
                                        static_cast<long long>(INFINITE - 1)));
}

bool overlapped_transfer(const HANDLE pipe,
                         void* buffer,
                         const DWORD size,
                         const bool write,
                         const Deadline deadline,
                         DWORD& transferred) {
    OVERLAPPED operation{};
    operation.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (operation.hEvent == nullptr) return false;
    transferred = 0;
    const BOOL started = write
                             ? WriteFile(pipe, buffer, size, &transferred, &operation)
                             : ReadFile(pipe, buffer, size, &transferred, &operation);
    if (!started && GetLastError() != ERROR_IO_PENDING) {
        CloseHandle(operation.hEvent);
        return false;
    }
    if (!started) {
        const DWORD remaining = remaining_milliseconds(deadline);
        const DWORD wait = remaining == 0 ? WAIT_TIMEOUT
                                          : WaitForSingleObject(operation.hEvent, remaining);
        if (wait != WAIT_OBJECT_0) {
            CancelIoEx(pipe, &operation);
            GetOverlappedResult(pipe, &operation, &transferred, TRUE);
            CloseHandle(operation.hEvent);
            SetLastError(wait == WAIT_TIMEOUT ? ERROR_TIMEOUT : ERROR_OPERATION_ABORTED);
            return false;
        }
        if (!GetOverlappedResult(pipe, &operation, &transferred, FALSE)) {
            CloseHandle(operation.hEvent);
            return false;
        }
    }
    CloseHandle(operation.hEvent);
    return transferred != 0;
}

bool write_all_with_deadline(const HANDLE pipe,
                             const std::string_view bytes,
                             const Deadline deadline) {
    std::size_t offset = 0;
    while (offset < bytes.size()) {
        const auto remaining = bytes.size() - offset;
        const auto chunk = static_cast<DWORD>((std::min)(
            remaining, static_cast<std::size_t>((std::numeric_limits<DWORD>::max)())));
        DWORD written = 0;
        if (!overlapped_transfer(pipe, const_cast<char*>(bytes.data() + offset),
                                 chunk, true, deadline, written)) return false;
        offset += written;
    }
    return true;
}

bool read_exact_with_deadline(const HANDLE pipe,
                              char* output,
                              const DWORD size,
                              const Deadline deadline) {
    DWORD offset = 0;
    while (offset < size) {
        DWORD read = 0;
        if (!overlapped_transfer(pipe, output + offset, size - offset,
                                 false, deadline, read)) return false;
        offset += read;
    }
    return true;
}

std::string read_frame_with_deadline(const HANDLE pipe, const Deadline deadline) {
    std::array<unsigned char, 4> prefix{};
    if (!read_exact_with_deadline(pipe, reinterpret_cast<char*>(prefix.data()), 4,
                                  deadline)) return {};
    const std::uint32_t size = static_cast<std::uint32_t>(prefix[0]) |
                               (static_cast<std::uint32_t>(prefix[1]) << 8U) |
                               (static_cast<std::uint32_t>(prefix[2]) << 16U) |
                               (static_cast<std::uint32_t>(prefix[3]) << 24U);
    if (size == 0 || size > protocol::kMaximumPayloadBytes) {
        SetLastError(ERROR_INVALID_DATA);
        return {};
    }
    std::string payload(size, '\0');
    if (!read_exact_with_deadline(pipe, payload.data(), size, deadline)) return {};
    return payload;
}

bool write_all(const HANDLE pipe, const std::string_view bytes) {
    std::size_t offset = 0;
    while (offset < bytes.size()) {
        const auto remaining = bytes.size() - offset;
        const auto chunk = static_cast<DWORD>((std::min)(
            remaining, static_cast<std::size_t>((std::numeric_limits<DWORD>::max)())));
        DWORD written = 0;
        if (!WriteFile(pipe, bytes.data() + offset, chunk, &written, nullptr) || written == 0) {
            return false;
        }
        offset += written;
    }
    return true;
}

bool read_exact(const HANDLE pipe, char* output, const DWORD size) {
    DWORD offset = 0;
    while (offset < size) {
        DWORD read = 0;
        if (!ReadFile(pipe, output + offset, size - offset, &read, nullptr) || read == 0) {
            return false;
        }
        offset += read;
    }
    return true;
}

bool write_frame(const HANDLE pipe, const std::string_view payload) {
    if (payload.empty() || payload.size() > protocol::kMaximumPayloadBytes) return false;
    return write_all(pipe, protocol::frame(payload));
}

std::string read_frame(const HANDLE pipe) {
    std::array<unsigned char, 4> prefix{};
    if (!read_exact(pipe, reinterpret_cast<char*>(prefix.data()), 4)) return {};
    const std::uint32_t size = static_cast<std::uint32_t>(prefix[0]) |
                               (static_cast<std::uint32_t>(prefix[1]) << 8U) |
                               (static_cast<std::uint32_t>(prefix[2]) << 16U) |
                               (static_cast<std::uint32_t>(prefix[3]) << 24U);
    if (size == 0 || size > protocol::kMaximumPayloadBytes) return {};
    std::string payload(size, '\0');
    if (!read_exact(pipe, payload.data(), size)) return {};
    return payload;
}

std::vector<std::string> preferred_source_segmentation(
    const engine::ParseResult& parsed,
    const std::vector<std::string>& candidate_segments,
    const bool correction_enabled) {
    const auto source_segments = [&parsed](const engine::ParsePath& path) {
        std::vector<std::string> segments;
        segments.reserve(path.syllables.size());
        for (const auto& syllable : path.syllables) {
            if (syllable.begin >= syllable.end ||
                syllable.end > parsed.normalized_input.size()) return std::vector<std::string>{};
            const auto source = std::string_view(parsed.normalized_input).substr(
                syllable.begin, syllable.end - syllable.begin);
            if (source.empty() || source.find('\'') != std::string_view::npos)
                return std::vector<std::string>{};
            segments.emplace_back(source);
        }
        return segments;
    };
    const auto raw_segments = [&parsed] {
        std::vector<std::string> segments;
        std::size_t begin = 0;
        while (begin < parsed.normalized_input.size()) {
            const auto end = parsed.normalized_input.find('\'', begin);
            const auto length = end == std::string::npos
                                    ? parsed.normalized_input.size() - begin
                                    : end - begin;
            if (length != 0) segments.push_back(parsed.normalized_input.substr(begin, length));
            if (end == std::string::npos) break;
            begin = end + 1;
        }
        return segments;
    };
    const auto exact = std::find_if(
        parsed.paths.begin(), parsed.paths.end(), [](const engine::ParsePath& path) {
            return path.match_kind == engine::InputMatchKind::exact && !path.syllables.empty();
        });
    // For long compact input the engine may intentionally compare two exact
    // segmentations. Keep the preview aligned with the winning candidate;
    // short input and user-authored apostrophe boundaries retain the existing
    // preview rules.
    if (parsed.normalized_input.size() >= 12 &&
        parsed.normalized_input.find('\'') == std::string::npos &&
        !candidate_segments.empty())
        return candidate_segments;
    if (!correction_enabled)
        return exact != parsed.paths.end() ? source_segments(*exact) : raw_segments();

    const auto preferred = std::find_if(
        parsed.paths.begin(), parsed.paths.end(), [](const engine::ParsePath& path) {
            return path.match_kind == engine::InputMatchKind::exact &&
                   !path.syllables.empty() &&
                   std::any_of(path.syllables.begin(), path.syllables.end(),
                               [](const engine::Syllable& syllable) {
                                   return !syllable.complete;
                               });
        });
    if (preferred == parsed.paths.end())
        return candidate_segments.empty() ? raw_segments() : std::vector<std::string>{};

    auto segments = source_segments(*preferred);
    if (segments.empty()) return {};
    if (candidate_segments.empty()) return segments;

    std::size_t divergence = 0;
    while (divergence < segments.size() && divergence < candidate_segments.size() &&
           segments[divergence] == candidate_segments[divergence])
        ++divergence;
    if (divergence == segments.size() && divergence == candidate_segments.size()) return {};
    if (divergence >= preferred->syllables.size() ||
        !preferred->syllables[divergence].complete ||
        segments[divergence].size() < 2)
        return {};
    return segments;
}

// Model ranking is latency-sensitive but never worth an unbounded thread per
// keystroke. Keep at most one queued task; a newer composition supersedes a
// task that has not started yet. One already-running model IPC can finish, but
// its result is discarded by the generation/request map below.
class LatestModelTaskQueue {
public:
    using Task = std::function<model::ModelMessage()>;

    LatestModelTaskQueue()
        : worker_([this](const std::stop_token stop) { run(stop); }) {}

    ~LatestModelTaskQueue() {
        worker_.request_stop();
        ready_.notify_all();
    }

    std::future<model::ModelMessage> submit(Task task) {
        Job next{std::move(task), {}};
        auto future = next.completion.get_future();
        {
            std::lock_guard lock(mutex_);
            if (pending_) pending_->completion.set_value({});
            pending_ = std::move(next);
        }
        ready_.notify_one();
        return future;
    }

private:
    struct Job {
        Task task;
        std::promise<model::ModelMessage> completion;
    };

    void run(const std::stop_token stop) {
        while (!stop.stop_requested()) {
            std::optional<Job> job;
            {
                std::unique_lock lock(mutex_);
                ready_.wait(lock, stop, [this] { return pending_.has_value(); });
                if (stop.stop_requested()) break;
                job = std::move(pending_);
                pending_.reset();
            }
            try {
                job->completion.set_value(job->task());
            } catch (...) {
                job->completion.set_value({});
            }
        }
        std::lock_guard lock(mutex_);
        if (pending_) {
            pending_->completion.set_value({});
            pending_.reset();
        }
    }

    std::mutex mutex_;
    std::condition_variable_any ready_;
    std::optional<Job> pending_;
    std::jthread worker_;
};

struct CandidateCacheKey {
    std::string input;
    std::string context_input;
    std::string context;
    std::uint64_t config_generation{};
    std::uint64_t learning_generation{};
    std::size_t result_limit{};
    std::size_t parse_path_limit{};
    bool correction_enabled{};
    bool contextual_ranking{};

    bool operator==(const CandidateCacheKey&) const = default;
};

struct CandidateCacheKeyHash {
    std::size_t operator()(const CandidateCacheKey& key) const noexcept {
        auto value = std::hash<std::string>{}(key.input);
        const auto combine = [&value](const std::size_t next) {
            value ^= next + 0x9e3779b9U + (value << 6U) + (value >> 2U);
        };
        combine(std::hash<std::string>{}(key.context_input));
        combine(std::hash<std::string>{}(key.context));
        combine(std::hash<std::uint64_t>{}(key.config_generation));
        combine(std::hash<std::uint64_t>{}(key.learning_generation));
        combine(key.result_limit);
        combine(key.parse_path_limit);
        combine(key.correction_enabled);
        combine(key.contextual_ranking);
        return value;
    }
};

struct CachedCandidateResult {
    engine::ParseResult parsed;
    std::vector<engine::Candidate> candidates;
};

class CandidateResultCache {
public:
    [[nodiscard]] std::optional<CachedCandidateResult> get(
        const CandidateCacheKey& key) {
        const auto found = entries_.find(key);
        if (found == entries_.end()) return std::nullopt;
        values_.splice(values_.begin(), values_, found->second);
        return found->second->second;
    }

    void put(CandidateCacheKey key, CachedCandidateResult value) {
        if (const auto found = entries_.find(key); found != entries_.end()) {
            found->second->second = std::move(value);
            values_.splice(values_.begin(), values_, found->second);
            return;
        }
        values_.emplace_front(std::move(key), std::move(value));
        entries_.emplace(values_.front().first, values_.begin());
        if (values_.size() <= kCapacity) return;
        entries_.erase(values_.back().first);
        values_.pop_back();
    }

private:
    static constexpr std::size_t kCapacity = 128;
    using ValueList = std::list<std::pair<CandidateCacheKey, CachedCandidateResult>>;
    ValueList values_;
    std::unordered_map<CandidateCacheKey, ValueList::iterator,
                       CandidateCacheKeyHash> entries_;
};

bool is_double_initial_input(const std::string_view input) {
    if (input.size() != 2) return false;
    constexpr std::string_view vowels = "aeiouv";
    return std::all_of(input.begin(), input.end(), [vowels](char value) {
        if (value >= 'A' && value <= 'Z')
            value = static_cast<char>(value - 'A' + 'a');
        return value >= 'a' && value <= 'z' &&
               vowels.find(value) == std::string_view::npos;
    });
}

std::vector<std::size_t> utf8_boundaries(const std::string_view text) {
    std::vector<std::size_t> boundaries{0};
    for (std::size_t index = 0; index < text.size(); ++index) {
        const auto byte = static_cast<unsigned char>(text[index]);
        if ((byte & 0xc0U) != 0x80U && index != 0) boundaries.push_back(index);
    }
    boundaries.push_back(text.size());
    return boundaries;
}

std::string_view utf8_prefix(const std::string_view text,
                             const std::size_t characters) {
    const auto boundaries = utf8_boundaries(text);
    if (boundaries.size() <= 1) return {};
    const auto count = std::min(characters, boundaries.size() - 1);
    return text.substr(0, boundaries[count]);
}

std::string_view utf8_suffix(const std::string_view text,
                             const std::size_t characters) {
    const auto boundaries = utf8_boundaries(text);
    if (boundaries.size() <= 1) return {};
    const auto count = std::min(characters, boundaries.size() - 1);
    return text.substr(boundaries[boundaries.size() - 1 - count]);
}

struct ContextualLexiconMatch {
    std::string candidate;
    std::size_t combined_syllables{};
    std::uint32_t frequency{};
    std::size_t original_index{};
};

std::vector<std::string> contextual_lexicon_preferences(
    const engine::Lexicon& lexicon,
    const engine::ParseResult& previous,
    const std::string_view context,
    const std::vector<engine::Candidate>& candidates,
    const std::size_t limit = 2) {
    if (!previous.valid || context.empty() || candidates.empty() || limit == 0)
        return {};
    const auto previous_path = std::find_if(
        previous.paths.begin(), previous.paths.end(),
        [](const engine::ParsePath& path) {
            return path.match_kind == engine::InputMatchKind::exact &&
                   !path.syllables.empty();
        });
    if (previous_path == previous.paths.end()) return {};

    std::vector<ContextualLexiconMatch> matches;
    for (std::size_t candidate_index = 0; candidate_index < candidates.size();
         ++candidate_index) {
        const auto& candidate = candidates[candidate_index];
        if (candidate.model_only || candidate.text.empty() ||
            candidate.syllables.empty()) continue;
        std::optional<ContextualLexiconMatch> best;
        const auto previous_limit = std::min<std::size_t>(
            2, previous_path->syllables.size());
        const auto candidate_limit = std::min<std::size_t>(
            2, std::min(candidate.syllables.size(),
                        utf8_character_count(candidate.text)));
        for (std::size_t previous_count = 1; previous_count <= previous_limit;
             ++previous_count) {
            const auto previous_text = utf8_suffix(context, previous_count);
            if (utf8_character_count(previous_text) != previous_count) continue;
            for (std::size_t candidate_count = 1; candidate_count <= candidate_limit;
                 ++candidate_count) {
                std::vector<std::string_view> reading;
                reading.reserve(previous_count + candidate_count);
                const auto previous_begin =
                    previous_path->syllables.size() - previous_count;
                for (std::size_t index = previous_begin;
                     index < previous_path->syllables.size(); ++index)
                    reading.push_back(previous_path->syllables[index].text);
                for (std::size_t index = 0; index < candidate_count; ++index)
                    reading.push_back(candidate.syllables[index]);
                const std::string expected = std::string(previous_text) +
                    std::string(utf8_prefix(candidate.text, candidate_count));
                for (const auto& entry : lexicon.lookup(reading)) {
                    if (entry.text != expected) continue;
                    const ContextualLexiconMatch current{
                        candidate.text, reading.size(), entry.frequency,
                        candidate_index};
                    if (!best || current.combined_syllables > best->combined_syllables ||
                        (current.combined_syllables == best->combined_syllables &&
                         current.frequency > best->frequency))
                        best = current;
                }
            }
        }
        if (best) matches.push_back(std::move(*best));
    }
    std::stable_sort(matches.begin(), matches.end(),
                     [](const auto& left, const auto& right) {
        if (left.combined_syllables != right.combined_syllables)
            return left.combined_syllables > right.combined_syllables;
        if (left.frequency != right.frequency)
            return left.frequency > right.frequency;
        return left.original_index < right.original_index;
    });
    std::vector<std::string> preferences;
    for (const auto& match : matches) {
        if (std::find(preferences.begin(), preferences.end(), match.candidate) !=
            preferences.end()) continue;
        preferences.push_back(match.candidate);
        if (preferences.size() == limit) break;
    }
    return preferences;
}

void promote_contextual_preferences(
    const std::vector<std::string>& preferences,
    std::vector<engine::Candidate>& candidates) {
    for (auto preference = preferences.rbegin(); preference != preferences.rend();
         ++preference) {
        const auto found = std::find_if(
            candidates.begin(), candidates.end(), [&preference](const engine::Candidate& candidate) {
                return candidate.text == *preference;
            });
        if (found == candidates.end()) continue;
        const auto consumed = found->consumed_input_bytes;
        auto tier_begin = std::find_if(
            candidates.begin(), found, [consumed](const engine::Candidate& candidate) {
                return candidate.consumed_input_bytes == consumed;
            });
        if (tier_begin == found) continue;
        auto promoted = std::move(*found);
        candidates.erase(found);
        candidates.insert(tier_begin, std::move(promoted));
    }
}

void arrange_double_initial_pages(std::vector<engine::Candidate>& candidates,
                                  const std::size_t page_size,
                                  const std::size_t full_input_bytes) {
    if (page_size == 0 || candidates.empty()) return;
    std::vector<engine::Candidate> dictionary;
    std::vector<engine::Candidate> prefixes;
    dictionary.reserve(candidates.size());
    prefixes.reserve(candidates.size());
    for (auto& candidate : candidates) {
        if (candidate.consumed_input_bytes == full_input_bytes)
            dictionary.push_back(std::move(candidate));
        else
            prefixes.push_back(std::move(candidate));
    }

    const auto left_count = (page_size + 1) / 2;
    const auto right_count = page_size / 2;
    std::vector<engine::Candidate> paged;
    paged.reserve(dictionary.size() + prefixes.size());
    std::size_t dictionary_index = 0;
    std::size_t prefix_index = 0;
    while (dictionary_index < dictionary.size() || prefix_index < prefixes.size()) {
        const auto dictionary_end = std::min(dictionary.size(),
                                             dictionary_index + left_count);
        while (dictionary_index < dictionary_end)
            paged.push_back(std::move(dictionary[dictionary_index++]));
        const auto prefix_end = std::min(prefixes.size(), prefix_index + right_count);
        while (prefix_index < prefix_end)
            paged.push_back(std::move(prefixes[prefix_index++]));

        // If one source is exhausted, keep the configured page length stable
        // by filling its unused slots from the remaining source. Normal pages
        // with both sources available always retain ceil/2 on the left and
        // floor/2 on the right.
        const auto row_remainder = paged.size() % page_size;
        if (row_remainder != 0 &&
            (dictionary_index == dictionary.size() || prefix_index == prefixes.size())) {
            auto missing = page_size - row_remainder;
            while (missing != 0 && dictionary_index < dictionary.size()) {
                paged.push_back(std::move(dictionary[dictionary_index++]));
                --missing;
            }
            while (missing != 0 && prefix_index < prefixes.size()) {
                paged.push_back(std::move(prefixes[prefix_index++]));
                --missing;
            }
        }
    }
    candidates = std::move(paged);
}

HANDLE open_pipe_with_deadline(const wchar_t* pipe_name,
                               const std::chrono::steady_clock::time_point deadline) {
    for (;;) {
        const DWORD remaining = remaining_milliseconds(deadline);
        if (remaining == 0) {
            SetLastError(ERROR_SEM_TIMEOUT);
            return INVALID_HANDLE_VALUE;
        }
        if (!WaitNamedPipeW(pipe_name, remaining)) {
            const DWORD error = GetLastError();
            if (error != ERROR_FILE_NOT_FOUND) return INVALID_HANDLE_VALUE;
            Sleep((std::min)(remaining, 2UL));
            continue;
        }
        const HANDLE pipe = CreateFileW(pipe_name, GENERIC_READ | GENERIC_WRITE, 0, nullptr,
                                        OPEN_EXISTING, FILE_FLAG_OVERLAPPED, nullptr);
        if (pipe != INVALID_HANDLE_VALUE) return pipe;
        const DWORD error = GetLastError();
        if (error != ERROR_PIPE_BUSY && error != ERROR_FILE_NOT_FOUND)
            return INVALID_HANDLE_VALUE;
        Sleep((std::min)(remaining, 2UL));
    }
}

}  // namespace

ExchangeResult exchange(const wchar_t* pipe_name,
                        const std::string_view request,
                        const std::chrono::milliseconds timeout) {
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    const HANDLE pipe = open_pipe_with_deadline(pipe_name, deadline);
    if (pipe == INVALID_HANDLE_VALUE) return {io_error("CreateFileW"), {}};

    if (request.empty() || request.size() > protocol::kMaximumPayloadBytes ||
        !write_all_with_deadline(pipe, protocol::frame(request), deadline)) {
        const auto status = io_error("WriteFile");
        CloseHandle(pipe);
        return {status, {}};
    }
    auto response = read_frame_with_deadline(pipe, deadline);
    if (response.empty()) {
        const auto status = io_error("ReadFile");
        CloseHandle(pipe);
        return {status, {}};
    }
    CloseHandle(pipe);
    return {{}, std::move(response)};
}

PersistentPipeClient::PersistentPipeClient(std::wstring pipe_name)
    : pipe_name_(std::move(pipe_name)) {}

PersistentPipeClient::~PersistentPipeClient() { reset(); }

void PersistentPipeClient::reset() noexcept {
    if (pipe_ == nullptr) return;
    CloseHandle(static_cast<HANDLE>(pipe_));
    pipe_ = nullptr;
}

ExchangeResult PersistentPipeClient::exchange(
    const std::string_view request, const std::chrono::milliseconds timeout) {
    if (request.empty() || request.size() > protocol::kMaximumPayloadBytes)
        return {{protocol::ErrorCode::invalid_payload, "invalid request size"}, {}};
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    for (int attempt = 0; attempt < 2; ++attempt) {
        if (pipe_ == nullptr) {
            const HANDLE opened = open_pipe_with_deadline(pipe_name_.c_str(), deadline);
            if (opened == INVALID_HANDLE_VALUE) return {io_error("CreateFileW"), {}};
            pipe_ = opened;
        }
        const HANDLE pipe = static_cast<HANDLE>(pipe_);
        if (write_all_with_deadline(pipe, protocol::frame(request), deadline)) {
            auto response = read_frame_with_deadline(pipe, deadline);
            if (!response.empty()) return {{}, std::move(response)};
        }
        const auto status = io_error("persistent pipe exchange");
        reset();
        if (attempt != 0 || remaining_milliseconds(deadline) == 0)
            return {status, {}};
    }
    return {io_error("persistent pipe exchange"), {}};
}

int run_core_server(const wchar_t* pipe_name, const engine::Lexicon& lexicon,
                    engine::UserFrequencyStore* user_frequency,
                    const wchar_t* model_pipe_name,
                    const config::ConfigMonitor* config_monitor) {
    const engine::FullPinyinSchema schema;
    const engine::CandidateGenerator generator(lexicon, nullptr, user_frequency);
    std::size_t unflushed_selections = 0;
    int exit_code = 0;
    bool running = true;
    struct PendingModelRequest {
        std::future<model::ModelMessage> future;
        std::chrono::steady_clock::time_point created;
        std::vector<std::string> candidates;
        std::vector<std::uint64_t> candidate_consumed;
        std::size_t full_input_bytes{};
        std::size_t visible_count{};
        std::vector<std::string> contextual_preferences;
        std::vector<std::string> learned_preferences;
    };
    std::unordered_map<std::string, PendingModelRequest> model_requests;
    LatestModelTaskQueue model_queue;
    CandidateResultCache candidate_cache;
    engine::FullPinyinIncrementalState incremental_parse_state;
    std::uint64_t learning_generation = 0;
    const auto model_key = [](const std::uint64_t request_id, const std::uint64_t generation) {
        return std::to_string(request_id) + ':' + std::to_string(generation);
    };
    CurrentUserPipeSecurity pipe_security;
    if (!pipe_security) return 2;
    const HANDLE ready_event = std::wstring_view(pipe_name) == kCorePipeName
                                   ? CreateEventW(nullptr, TRUE, FALSE,
                                                  L"Local\\OwO.InputMethod.Core.Ready.P1")
                                   : nullptr;
    bool ready_signalled = false;
    while (running) {
        const HANDLE pipe = CreateNamedPipeW(
            pipe_name, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, protocol::kMaximumPayloadBytes + 4U, protocol::kMaximumPayloadBytes + 4U,
            5000, pipe_security.get());
        if (pipe == INVALID_HANDLE_VALUE) {
            if (ready_event != nullptr) CloseHandle(ready_event);
            return 2;
        }
        if (!ready_signalled) {
            if (ready_event != nullptr) SetEvent(ready_event);
            ready_signalled = true;
        }

        const BOOL connected = ConnectNamedPipe(pipe, nullptr) || GetLastError() == ERROR_PIPE_CONNECTED;
        if (!connected) {
            CloseHandle(pipe);
            continue;
        }

        while (running) {
        const auto request_json = read_frame(pipe);
        if (request_json.empty()) break;
        const auto decoded = protocol::decode_message(request_json);
        const auto now = std::chrono::steady_clock::now();
        for (auto pending = model_requests.begin(); pending != model_requests.end();) {
            if (now - pending->second.created > std::chrono::seconds(5) &&
                pending->second.future.wait_for(std::chrono::milliseconds(0)) ==
                    std::future_status::ready)
                pending = model_requests.erase(pending);
            else
                ++pending;
        }
        protocol::Message response{};
        if (!decoded.validation) {
            std::clog << R"({"process":"core_service","module":"ipc","level":"error","event_id":"invalid_request"})"
                      << '\n';
            response.type = protocol::MessageType::error_response;
            response.text = decoded.validation.message;
        } else {
            const auto runtime_config = config_monitor != nullptr
                                            ? config_monitor->snapshot()
                                            : std::shared_ptr<const config::AppConfig>{};
            const bool adaptive_ranking_enabled = runtime_config != nullptr &&
                runtime_config->model_ranking_enabled;
            const bool external_model_ranking_enabled = model_pipe_name != nullptr &&
                (runtime_config == nullptr || runtime_config->model_ranking_enabled);
            const bool user_learning_enabled = runtime_config == nullptr ||
                runtime_config->user_learning_enabled;
            if (user_frequency != nullptr)
                user_frequency->set_sensitivity(runtime_config != nullptr
                    ? runtime_config->user_learning_sensitivity : 7U);
            const auto model_timeout = runtime_config != nullptr
                                           ? runtime_config->model_timeout_ms
                                           : 50U;
            const auto candidate_page_size = runtime_config != nullptr
                                                 ? runtime_config->candidate_page_size
                                                 : 5U;
            response.request_id = decoded.message.request_id;
            response.context_generation = decoded.message.context_generation;
            if (decoded.message.type == protocol::MessageType::shutdown_request) {
                std::clog << R"({"process":"core_service","module":"lifecycle","level":"info","event_id":"shutdown_requested"})"
                          << '\n';
                if (user_frequency != nullptr && unflushed_selections != 0) {
                    const auto flushed = user_frequency->flush();
                    if (!flushed.success) {
                        response.type = protocol::MessageType::error_response;
                        response.text = flushed.error;
                        exit_code = 3;
                    } else {
                        response.type = protocol::MessageType::acknowledgement;
                        response.text = "shutdown_ack";
                    }
                } else {
                    response.type = protocol::MessageType::acknowledgement;
                    response.text = "shutdown_ack";
                }
                running = false;
            } else if (decoded.message.type == protocol::MessageType::candidate_update_request) {
                response.type = protocol::MessageType::candidate_update_response;
                if (!external_model_ranking_enabled) {
                    const auto key = model_key(decoded.message.request_id,
                                               decoded.message.context_generation);
                    const auto found = model_requests.find(key);
                    if (found != model_requests.end() &&
                        found->second.future.wait_for(std::chrono::milliseconds(0)) ==
                            std::future_status::ready)
                        model_requests.erase(found);
                } else {
                const auto key = model_key(decoded.message.request_id,
                                           decoded.message.context_generation);
                const auto found = model_requests.find(key);
                if (found != model_requests.end() &&
                    found->second.future.wait_for(std::chrono::milliseconds(0)) !=
                        std::future_status::ready) {
                    // Consolidate the former sequence of short TSF polls into
                    // one bounded update wait. Base candidates were already
                    // returned by candidate_response.
                    found->second.future.wait_for(std::chrono::milliseconds(
                        std::min<std::uint32_t>(model_timeout, 60U)));
                }
                if (found != model_requests.end() &&
                    found->second.future.wait_for(std::chrono::milliseconds(0)) !=
                        std::future_status::ready) {
                    response.model_pending = true;
                } else if (found != model_requests.end()) {
                    auto pending = std::move(found->second);
                    model_requests.erase(found);
                    auto update = pending.future.get();
                    if (update.type == model::ModelMessageType::rank_response &&
                        update.status == model::ModelStatus::success) {
                        apply_contextual_candidate_order(
                            update.candidates, pending.full_input_bytes,
                            pending.candidates, pending.candidate_consumed,
                            response.candidates, response.candidate_consumed);
                        std::string model_preferred_full;
                        for (std::size_t index = 0;
                             index < response.candidates.size(); ++index) {
                            if (response.candidate_consumed[index] !=
                                pending.full_input_bytes)
                                continue;
                            model_preferred_full = response.candidates[index];
                            break;
                        }
                        promote_preferred_candidates(
                            pending.contextual_preferences, response.candidates,
                            response.candidate_consumed);
                        restore_preferred_candidate_positions(
                            pending.learned_preferences, pending.candidates,
                            pending.candidate_consumed, response.candidates,
                            response.candidate_consumed);
                        auto visible_full_count = std::max<std::size_t>(
                            1, static_cast<std::size_t>(std::count(
                                   pending.candidate_consumed.begin(),
                                   pending.candidate_consumed.begin() +
                                       static_cast<std::ptrdiff_t>(
                                           std::min(pending.visible_count,
                                            pending.candidate_consumed.size())),
                                   pending.full_input_bytes)));
                        // A learned choice may disagree with the language
                        // model. Preserve both full-input candidates in that
                        // case so the user can select the model preference and
                        // naturally correct their frequency history. Inputs
                        // without such a conflict still expose only the normal
                        // one-or-two sentence budget.
                        const auto learned_preferred_full = std::find_if(
                            response.candidate_consumed.begin(),
                            response.candidate_consumed.end(),
                            [&pending](const std::uint64_t consumed) {
                                return consumed == pending.full_input_bytes;
                            });
                        if (!model_preferred_full.empty() &&
                            learned_preferred_full !=
                                response.candidate_consumed.end()) {
                            const auto index = static_cast<std::size_t>(
                                learned_preferred_full -
                                response.candidate_consumed.begin());
                            if (response.candidates[index] != model_preferred_full)
                                visible_full_count = std::max<std::size_t>(
                                    visible_full_count, 2);
                        }
                        std::size_t retained_full = 0;
                        for (std::size_t index = 0;
                             index < response.candidates.size();) {
                            if (response.candidate_consumed[index] !=
                                pending.full_input_bytes ||
                                retained_full++ < visible_full_count) {
                                ++index;
                                continue;
                            }
                            response.candidates.erase(
                                response.candidates.begin() +
                                static_cast<std::ptrdiff_t>(index));
                            response.candidate_consumed.erase(
                                response.candidate_consumed.begin() +
                                static_cast<std::ptrdiff_t>(index));
                        }
                        if (response.candidates.size() > pending.visible_count) {
                            response.candidates.resize(pending.visible_count);
                            response.candidate_consumed.resize(pending.visible_count);
                        }
                    }
                }
                }
            } else if (decoded.message.type == protocol::MessageType::candidate_request) {
                const auto request_started = std::chrono::steady_clock::now();
                const auto cancellation_name = candidate_cancellation_event_name(
                    decoded.message.request_id, decoded.message.context_generation);
                const HANDLE cancellation_event = CreateEventW(
                    nullptr, TRUE, FALSE, cancellation_name.c_str());
                const auto cancelled = [cancellation_event] {
                    return cancellation_event != nullptr &&
                           WaitForSingleObject(cancellation_event, 0) == WAIT_OBJECT_0;
                };
                response.type = protocol::MessageType::candidate_response;
                constexpr std::uint64_t maximum_page = 100;
                constexpr std::size_t maximum_expanded_pages = 8;
                constexpr std::size_t maximum_expanded_candidates = 64;
                std::size_t full_input_bytes = 0;
                std::vector<std::string> model_candidate_pool;
                std::vector<std::size_t> model_candidate_consumed;
                std::vector<std::string> model_hidden_candidates;
                std::vector<std::uint64_t> model_hidden_consumed;
                std::vector<std::string> contextual_preferences;
                std::vector<std::string> learned_preferences;
                if (decoded.message.page > maximum_page ||
                    (decoded.message.expanded && decoded.message.page != 0)) {
                    response.type = protocol::MessageType::error_response;
                    response.text = decoded.message.expanded
                                        ? "expanded candidate request must start at page zero"
                                        : "candidate page exceeds limit";
                } else {
                    const auto page = static_cast<std::size_t>(decoded.message.page);
                    const auto page_size = static_cast<std::size_t>(candidate_page_size);
                    const auto expanded_pages = std::min(
                        maximum_expanded_pages, maximum_expanded_candidates / page_size);
                    const auto result_size = decoded.message.expanded
                                                 ? page_size * expanded_pages
                                                 : page_size;
                    const auto begin = decoded.message.expanded ? 0 : page * page_size;
                    std::uint64_t parse_phase_us = 0;
                    std::uint64_t generation_phase_us = 0;
                    const auto parse_path_limit = decoded.message.expanded
                                                      ? std::size_t{32}
                                                      : std::size_t{16};
                    engine::FullPinyinParseMetrics parse_metrics;
                    engine::CandidateGenerationMetrics generation_metrics;
                    const bool double_initial_request =
                        is_double_initial_input(decoded.message.text);
                    auto result_limit = begin + result_size + 1;
                    if (adaptive_ranking_enabled && !decoded.message.input.empty() &&
                        !decoded.message.context.empty())
                        result_limit = std::max<std::size_t>(result_limit, 16);
                    if (double_initial_request) {
                        const auto requested_pages = decoded.message.expanded
                                                         ? expanded_pages
                                                         : page + 1;
                        const auto left_per_page = (page_size + 1) / 2;
                        // The generator alternates its two independently
                        // ranked sources. Ask for enough of both to give every
                        // requested odd-sized page its extra left candidate,
                        // plus one dictionary look-ahead for has_more.
                        const auto required_left =
                            requested_pages * left_per_page + 1;
                        result_limit = std::max(result_limit, required_left * 2);
                    }
                    CandidateCacheKey cache_key{
                        decoded.message.text,
                        decoded.message.input,
                        decoded.message.context,
                        config_monitor != nullptr ? config_monitor->generation() : 0,
                        learning_generation,
                        result_limit,
                        parse_path_limit,
                        decoded.message.correction_enabled,
                        adaptive_ranking_enabled};
                    auto cached = candidate_cache.get(cache_key);
                    const bool cache_hit = cached.has_value();
                    engine::ParseResult parsed;
                    std::vector<engine::Candidate> candidates;
                    bool correction_fallback = false;
                    bool incremental_reused = false;
                    if (cached) {
                        parsed = std::move(cached->parsed);
                        candidates = std::move(cached->candidates);
                    } else {
                        const auto strict_parse_started = std::chrono::steady_clock::now();
                        const auto long_compact_input =
                            decoded.message.text.size() >= 12 &&
                            decoded.message.text.find('\'') == std::string::npos;
                        const auto strict_path_limit =
                            decoded.message.expanded || long_compact_input
                                ? std::size_t{16}
                                : std::size_t{8};
                        if (long_compact_input) {
                            // Long ambiguous input must be deterministic for a
                            // given string. Reusing a boundary fixed by an
                            // earlier prefix can lock kuang'ao and poison the
                            // candidate cache even after kuan'gao becomes the
                            // stronger complete sentence.
                            parsed = schema.parse(
                                decoded.message.text, strict_path_limit, false,
                                &parse_metrics, cancelled);
                            incremental_parse_state = {
                                decoded.message.text, parsed, strict_path_limit};
                        } else {
                            parsed = schema.parse_incremental(
                                decoded.message.text, strict_path_limit,
                                incremental_parse_state, &parse_metrics, cancelled,
                                &incremental_reused);
                        }
                        parse_phase_us += static_cast<std::uint64_t>(
                            std::chrono::duration_cast<std::chrono::microseconds>(
                                std::chrono::steady_clock::now() - strict_parse_started).count());
                        const auto strict_generation_started = std::chrono::steady_clock::now();
                        candidates = generator.generate(
                            parsed, result_limit, adaptive_ranking_enabled,
                            decoded.message.context, &generation_metrics, cancelled,
                            external_model_ranking_enabled);
                        generation_phase_us += static_cast<std::uint64_t>(
                            std::chrono::duration_cast<std::chrono::microseconds>(
                                std::chrono::steady_clock::now() -
                                strict_generation_started).count());
                        const auto strict_evidence = std::any_of(
                            candidates.begin(), candidates.end(),
                            [&parsed](const auto& candidate) {
                                if (candidate.consumed_input_bytes !=
                                    parsed.normalized_input.size()) return false;
                                if (candidate.match_kind == engine::InputMatchKind::exact)
                                    return true;
                                std::size_t abbreviated_segments = 0;
                                const auto shared = std::min(
                                    candidate.source_segments.size(),
                                    candidate.syllables.size());
                                for (std::size_t index = 0; index < shared; ++index) {
                                    if (candidate.source_segments[index].size() <
                                        candidate.syllables[index].size())
                                        ++abbreviated_segments;
                                }
                                // A short trailing prefix (nih -> ni+hao) is
                                // already useful. Longer compact forms require
                                // at least two independent abbreviation matches
                                // (bugd -> bu+gan+dang) before they suppress typo
                                // correction such as niaho -> ni+hao.
                                return candidate.source_segments.size() <= 2 ||
                                       abbreviated_segments >= 2;
                            });
                        correction_fallback = decoded.message.correction_enabled &&
                                              (candidates.size() < result_size ||
                                               !strict_evidence) &&
                                              !cancelled();
                        if (correction_fallback) {
                            engine::FullPinyinParseMetrics corrected_parse_metrics;
                            engine::CandidateGenerationMetrics corrected_generation_metrics;
                            const auto corrected_parse_started =
                                std::chrono::steady_clock::now();
                            auto corrected = schema.parse(
                                decoded.message.text, parse_path_limit, true,
                                &corrected_parse_metrics, cancelled);
                            parse_phase_us += static_cast<std::uint64_t>(
                                std::chrono::duration_cast<std::chrono::microseconds>(
                                    std::chrono::steady_clock::now() -
                                    corrected_parse_started).count());
                            const auto corrected_generation_started =
                                std::chrono::steady_clock::now();
                            auto corrected_candidates = generator.generate(
                                corrected, result_limit, adaptive_ranking_enabled,
                                decoded.message.context, &corrected_generation_metrics,
                                cancelled, external_model_ranking_enabled);
                            generation_phase_us += static_cast<std::uint64_t>(
                                std::chrono::duration_cast<std::chrono::microseconds>(
                                    std::chrono::steady_clock::now() -
                                    corrected_generation_started).count());
                            parse_metrics.normalization_us +=
                                corrected_parse_metrics.normalization_us;
                            parse_metrics.segmentation_us +=
                                corrected_parse_metrics.segmentation_us;
                            parse_metrics.correction_us +=
                                corrected_parse_metrics.correction_us;
                            generation_metrics.lexicon_lookup_us +=
                                corrected_generation_metrics.lexicon_lookup_us;
                            generation_metrics.lexicon_lookup_count +=
                                corrected_generation_metrics.lexicon_lookup_count;
                            generation_metrics.sort_us +=
                                corrected_generation_metrics.sort_us;
                            if (!corrected_candidates.empty()) {
                                parsed = std::move(corrected);
                                candidates = std::move(corrected_candidates);
                            }
                        }
                        if (!cancelled() && double_initial_request)
                            arrange_double_initial_pages(
                                candidates, page_size,
                                parsed.normalized_input.size());
                        if (!cancelled())
                            candidate_cache.put(cache_key, {parsed, candidates});
                    }
                    if (adaptive_ranking_enabled && !decoded.message.input.empty() &&
                        !decoded.message.context.empty() && !candidates.empty()) {
                        const auto previous = schema.parse(
                            decoded.message.input, 8, false);
                        contextual_preferences = contextual_lexicon_preferences(
                            lexicon, previous, decoded.message.context, candidates);
                        promote_contextual_preferences(
                            contextual_preferences, candidates);
                    }
                    full_input_bytes = parsed.normalized_input.size();
                    const auto generation_finished = std::chrono::steady_clock::now();
                    const auto first_model_only = std::find_if(
                        candidates.begin(), candidates.end(),
                        [](const engine::Candidate& candidate) {
                            return candidate.model_only;
                        });
                    const auto visible_candidate_count = static_cast<std::size_t>(
                        first_model_only - candidates.begin());
                    model_candidate_pool.reserve(candidates.size());
                    model_candidate_consumed.reserve(candidates.size());
                    for (const auto& candidate : candidates) {
                        model_candidate_pool.push_back(candidate.text);
                        model_candidate_consumed.push_back(
                            candidate.consumed_input_bytes);
                    }
                    if (begin == 0) {
                        for (std::size_t index = visible_candidate_count;
                             index < candidates.size(); ++index) {
                            if (!candidates[index].model_only) continue;
                            model_hidden_candidates.push_back(candidates[index].text);
                            model_hidden_consumed.push_back(
                                candidates[index].consumed_input_bytes);
                        }
                    }
                    const auto end = std::min(visible_candidate_count,
                                              begin + result_size);
                    response.page = decoded.message.expanded ? 0 : decoded.message.page;
                    response.expanded = decoded.message.expanded;
                    response.page_size = candidate_page_size;
                    response.correction_enabled = decoded.message.correction_enabled;
                    response.has_more = visible_candidate_count > end;
                    // Preview the parser's preferred source structure. Auxiliary
                    // abbreviation/correction paths may generate candidates, but
                    // must not rewrite xing+b as xin+g+b or xing+ba+f as xing+baf.
                    const auto request_cancelled = cancelled();
                    if (!request_cancelled) {
                        const auto candidate_segments = candidates.empty()
                                                            ? std::vector<std::string>{}
                                                            : candidates.front().source_segments;
                        response.syllables = preferred_source_segmentation(
                            parsed, candidate_segments, decoded.message.correction_enabled);
                        if (response.syllables.empty())
                            response.syllables = candidate_segments;
                    }
                    if (begin < candidates.size()) {
                        response.candidates.reserve(end - begin);
                        response.candidate_consumed.reserve(end - begin);
                        const auto append_candidate = [&response, &candidates](
                                                          const std::size_t index) {
                            response.candidates.push_back(candidates[index].text);
                            response.candidate_consumed.push_back(
                                candidates[index].consumed_input_bytes);
                        };
                        if (double_initial_request) {
                            // The generator alternates sources so every page
                            // receives an even share. Group each delivered page
                            // afterward so TSF can render dictionary words in
                            // the left half and first-initial characters in the
                            // right half with contiguous numeric shortcuts.
                            for (std::size_t row_begin = begin; row_begin < end;
                                 row_begin += page_size) {
                                const auto row_end = std::min(end, row_begin + page_size);
                                for (std::size_t index = row_begin; index < row_end; ++index) {
                                    if (candidates[index].consumed_input_bytes ==
                                        full_input_bytes)
                                        append_candidate(index);
                                }
                                for (std::size_t index = row_begin; index < row_end; ++index) {
                                    if (candidates[index].consumed_input_bytes !=
                                        full_input_bytes)
                                        append_candidate(index);
                                }
                            }
                        } else {
                            for (std::size_t index = begin; index < end; ++index)
                                append_candidate(index);
                        }
                    }
                    const auto parse_us = parse_phase_us;
                    const auto generation_us = generation_phase_us;
                    const auto total_us = std::chrono::duration_cast<std::chrono::microseconds>(
                        generation_finished - request_started).count();
                    std::clog
                        << R"({"process":"core_service","module":"candidate","level":"info","event_id":"candidate_generated","request_id":)"
                        << decoded.message.request_id
                        << R"(,"generation":)" << decoded.message.context_generation
                        << R"(,"input_bytes":)" << decoded.message.text.size()
                        << R"(,"parse_paths":)" << parsed.paths.size()
                        << R"(,"candidate_count":)" << candidates.size()
                        << R"(,"parse_us":)" << parse_us
                        << R"(,"normalization_us":)" << parse_metrics.normalization_us
                        << R"(,"segmentation_us":)" << parse_metrics.segmentation_us
                        << R"(,"correction_us":)" << parse_metrics.correction_us
                        << R"(,"generation_us":)" << generation_us
                        << R"(,"lexicon_lookup_us":)" << generation_metrics.lexicon_lookup_us
                        << R"(,"lexicon_lookup_count":)" << generation_metrics.lexicon_lookup_count
                        << R"(,"sort_us":)" << generation_metrics.sort_us
                        << R"(,"total_us":)" << total_us
                        << R"(,"correction_enabled":)"
                        << (decoded.message.correction_enabled ? "true" : "false")
                        << R"(,"expanded":)" << (decoded.message.expanded ? "true" : "false")
                        << R"(,"cancelled":)" << (request_cancelled ? "true" : "false")
                        << R"(,"cache_hit":)" << (cache_hit ? "true" : "false")
                        << R"(,"correction_fallback":)"
                        << (correction_fallback ? "true" : "false")
                        << R"(,"incremental_reused":)"
                        << (incremental_reused ? "true" : "false")
                        << "}\n";
                }
                // Transitional compatibility for the P1 TSF consumer. It is removed when
                // TSF owns a paged candidate list later in P2.1.
                if (!response.candidates.empty()) response.text = response.candidates.front();
                if (user_frequency != nullptr && user_learning_enabled &&
                    !response.candidates.empty()) {
                    struct LearnedPreference {
                        std::string text;
                        std::int64_t score{};
                        std::size_t index{};
                    };
                    std::vector<LearnedPreference> learned;
                    learned.reserve(response.candidates.size());
                    for (std::size_t index = 0; index < response.candidates.size(); ++index) {
                        const auto& text = response.candidates[index];
                        const auto score = user_frequency->score(text) +
                            user_frequency->contextual_score(
                                decoded.message.text, text) +
                            user_frequency->language_context_score(
                                decoded.message.context, decoded.message.text, text);
                        if (score > 0) learned.push_back({text, score, index});
                    }
                    std::stable_sort(learned.begin(), learned.end(),
                                     [](const auto& left, const auto& right) {
                        if (left.score != right.score) return left.score > right.score;
                        return left.index < right.index;
                    });
                    for (const auto& preference : learned) {
                        learned_preferences.push_back(preference.text);
                        if (learned_preferences.size() == 2) break;
                    }
                }
                auto model_update_candidates = response.candidates;
                auto model_update_consumed = response.candidate_consumed;
                // Normal candidates remain page-local so a delayed model
                // result cannot replace page N with page zero. Hidden
                // whole-input alternatives are the deliberate exception:
                // they exist only to let sentence-level scoring replace a
                // raw-frequency composition on the first page.
                model_update_candidates.insert(model_update_candidates.end(),
                                               model_hidden_candidates.begin(),
                                               model_hidden_candidates.end());
                model_update_consumed.insert(model_update_consumed.end(),
                                             model_hidden_consumed.begin(),
                                             model_hidden_consumed.end());
                if (external_model_ranking_enabled && !response.candidates.empty() &&
                    !is_double_initial_input(decoded.message.text) &&
                    model_requests.size() < 128) {
                    model::ModelMessage model_request;
                    model_request.type = model::ModelMessageType::rank_request;
                    model_request.status = model::ModelStatus::success;
                    model_request.request_id = decoded.message.request_id;
                    model_request.timeout_ms = model_timeout;
                    // An empty identifier selects the active ModelHost backend. Explicit
                    // identifiers remain available to diagnostic clients.
                    model_request.model_id.clear();
                    model_request.input = decoded.message.text;
                    model_request.context = decoded.message.context;
                    model_request.candidates = model_candidate_pool;
                    const std::wstring model_pipe(model_pipe_name);
                    // Only the newest composition can affect the active TSF
                    // window. Drop mappings for older generations before the
                    // latest-only worker accepts the new ranking task.
                    model_requests.clear();
                    model_requests.insert_or_assign(
                        model_key(decoded.message.request_id,
                                  decoded.message.context_generation),
                        PendingModelRequest{model_queue.submit(
                                   [model_pipe, request = std::move(model_request), model_timeout] {
                            const auto exchanged = exchange(
                                model_pipe.c_str(), model::encode_model_message(request),
                                std::chrono::milliseconds(model_timeout + 25U));
                            if (!exchanged.status) return model::ModelMessage{};
                            const auto decoded_model =
                                model::decode_model_message(exchanged.response);
                            return decoded_model.validation ? decoded_model.message
                                                            : model::ModelMessage{};
                        }), std::chrono::steady_clock::now(),
                            std::move(model_update_candidates),
                            std::move(model_update_consumed), full_input_bytes,
                            response.candidates.size(),
                            contextual_preferences, learned_preferences});
                    response.model_pending = true;
                }
                if (cancellation_event != nullptr) CloseHandle(cancellation_event);
            } else if (decoded.message.type == protocol::MessageType::candidate_committed) {
                response.type = protocol::MessageType::acknowledgement;
                response.text = "commit_ack";
                if (user_frequency != nullptr && user_learning_enabled &&
                    !decoded.message.text.empty()) {
                    const auto feedback = schema.parse(decoded.message.input, 1, false);
                    if (feedback.valid)
                        user_frequency->record(decoded.message.context,
                                               feedback.normalized_input,
                                               decoded.message.text);
                    else
                        user_frequency->record(decoded.message.text);
                    ++learning_generation;
                    ++unflushed_selections;
                }
                if (unflushed_selections >= 32) {
                    const auto flushed = user_frequency->flush();
                    if (flushed.success) unflushed_selections = 0;
                    else {
                        response.type = protocol::MessageType::error_response;
                        response.text = flushed.error;
                    }
                }
            } else {
                response.type = protocol::MessageType::error_response;
                response.text = "unsupported request type";
            }
        }

        const auto encoded = protocol::encode_message(response);
        if (!encoded.empty()) write_frame(pipe, encoded);
        FlushFileBuffers(pipe);
        }
        DisconnectNamedPipe(pipe);
        CloseHandle(pipe);
    }
    if (ready_event != nullptr) CloseHandle(ready_event);
    return exit_code;
}

int run_core_server(const wchar_t* pipe_name, const engine::Lexicon& lexicon,
                    engine::UserFrequencyStore* user_frequency,
                    const wchar_t* model_pipe_name) {
    return run_core_server(pipe_name, lexicon, user_frequency, model_pipe_name, nullptr);
}

int run_core_server(const wchar_t* pipe_name, const engine::Lexicon& lexicon,
                    engine::UserFrequencyStore* user_frequency) {
    return run_core_server(pipe_name, lexicon, user_frequency, nullptr);
}

int run_core_server(const wchar_t* pipe_name, const engine::Lexicon& lexicon) {
    return run_core_server(pipe_name, lexicon, nullptr);
}

int run_core_server(const wchar_t* pipe_name) {
    const engine::MemoryLexicon fallback({
        {{"ni", "hao"}, "你好", 1000},
        {{"ni", "hao"}, "你号", 50},
        {{"xian"}, "先", 800},
        {{"xian"}, "线", 700},
        {{"xi", "an"}, "西安", 900},
    });
    return run_core_server(pipe_name, fallback);
}

int run_model_server(const wchar_t* pipe_name, model::IModelBackend& backend) {
    bool running = true;
    bool ready_logged = false;
    CurrentUserPipeSecurity pipe_security;
    if (!pipe_security) return 2;
    while (running) {
        const HANDLE pipe = CreateNamedPipeW(
            pipe_name, PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1, protocol::kMaximumPayloadBytes + 4U, protocol::kMaximumPayloadBytes + 4U,
            5000, pipe_security.get());
        if (pipe == INVALID_HANDLE_VALUE) return 2;
        if (!ready_logged) {
            std::clog << R"({"process":"model_host","module":"startup","level":"info","event_id":"model_pipe_ready"})"
                      << '\n';
            ready_logged = true;
        }
        const BOOL connected = ConnectNamedPipe(pipe, nullptr) || GetLastError() == ERROR_PIPE_CONNECTED;
        if (!connected) {
            CloseHandle(pipe);
            continue;
        }

        while (running) {
        const auto request_json = read_frame(pipe);
        if (request_json.empty()) break;
        const auto decoded = model::decode_model_message(request_json);
        model::ModelMessage response;
        if (!decoded.validation) {
            response.type = model::ModelMessageType::error_response;
            response.status = model::ModelStatus::backend_error;
            response.diagnostic = decoded.validation.message;
        } else {
            response.request_id = decoded.message.request_id;
            if (decoded.message.type == model::ModelMessageType::shutdown_request) {
                response.type = model::ModelMessageType::acknowledgement;
                response.status = model::ModelStatus::success;
                running = false;
            } else if (decoded.message.type == model::ModelMessageType::rank_request) {
                if (!decoded.message.model_id.empty() &&
                    decoded.message.model_id != backend.id()) {
                    response.type = model::ModelMessageType::error_response;
                    response.status = model::ModelStatus::backend_error;
                    response.diagnostic = "model backend is unavailable";
                } else {
                    const auto rank_started = std::chrono::steady_clock::now();
                    model::ModelRequest request;
                    request.request_id = decoded.message.request_id;
                    request.model_id = decoded.message.model_id.empty()
                                           ? std::string(backend.id())
                                           : decoded.message.model_id;
                    request.input = decoded.message.input;
                    request.context = decoded.message.context;
                    request.candidates = decoded.message.candidates;
                    request.timeout = std::chrono::milliseconds(decoded.message.timeout_ms);
                    auto result = backend.rank(request, {});
                    const auto rank_us = std::chrono::duration_cast<std::chrono::microseconds>(
                        std::chrono::steady_clock::now() - rank_started).count();
                    std::clog
                        << R"({"process":"model_host","module":"ranking","level":"info","event_id":"candidates_ranked","request_id":)"
                        << decoded.message.request_id
                        << R"(,"candidate_count":)" << decoded.message.candidates.size()
                        << R"(,"rank_us":)" << rank_us
                        << R"(,"status":)" << static_cast<unsigned>(result.status)
                        << "}\n";
                    response.type = model::ModelMessageType::rank_response;
                    response.status = result.status;
                    response.candidates = std::move(result.candidates);
                    response.diagnostic = std::move(result.diagnostic);
                }
            } else {
                response.type = model::ModelMessageType::error_response;
                response.status = model::ModelStatus::backend_error;
                response.diagnostic = "unsupported model message type";
            }
        }
        const auto encoded = model::encode_model_message(response);
        if (!encoded.empty()) write_frame(pipe, encoded);
        FlushFileBuffers(pipe);
        }
        DisconnectNamedPipe(pipe);
        CloseHandle(pipe);
    }
    return 0;
}

}  // namespace owo::ipc
