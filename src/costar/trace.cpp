#include "costar/trace.hpp"

#include <algorithm>
#include <charconv>
#include <format>
#include <optional>

namespace costar {

std::vector<TraceRecord> Trace::for_machine(uint32_t machine) const {
  std::vector<TraceRecord> out;
  std::ranges::copy_if(records_, std::back_inserter(out),
                       [machine](const TraceRecord& r) { return r.machine == machine; });
  return out;
}

namespace {

bool label_matches(std::string_view pattern, std::string_view label) {
  if (pattern.ends_with('*')) return label.starts_with(pattern.substr(0, pattern.size() - 1));
  return pattern == label;
}

// The length of the UTF-8 sequence at the start of `text`, or 0 if it isn't
// one (overlong forms and surrogates aren't).
size_t utf8_length(std::string_view text) {
  const auto byte = [&](size_t i) { return static_cast<unsigned char>(text[i]); };
  const unsigned char c = byte(0);
  size_t length = 0;
  unsigned char low = 0x80, high = 0xbf;  // the second byte's range
  if (c >= 0xc2 && c <= 0xdf) {
    length = 2;
  } else if (c >= 0xe0 && c <= 0xef) {
    length = 3;
    low = c == 0xe0 ? 0xa0 : 0x80;
    high = c == 0xed ? 0x9f : 0xbf;
  } else if (c >= 0xf0 && c <= 0xf4) {
    length = 4;
    low = c == 0xf0 ? 0x90 : 0x80;
    high = c == 0xf4 ? 0x8f : 0xbf;
  }
  if (length == 0 || text.size() < length || byte(1) < low || byte(1) > high) return 0;
  for (size_t i = 2; i < length; ++i) {
    if ((byte(i) & 0xc0) != 0x80) return 0;
  }
  return length;
}

// `text` as a JSON string.  Bytes that aren't valid UTF-8 (firmware can log
// anything) become the code points of the same value, so the output is
// always valid JSON.
void append_json_string(std::string& out, std::string_view text) {
  static constexpr char kDigits[] = "0123456789abcdef";
  out += '"';
  for (size_t i = 0; i < text.size(); ++i) {
    const auto c = static_cast<unsigned char>(text[i]);
    if (c == '"' || c == '\\') {
      out += '\\';
      out += static_cast<char>(c);
    } else if (c == '\n') {
      out += "\\n";
    } else if (c == '\r') {
      out += "\\r";
    } else if (c == '\t') {
      out += "\\t";
    } else if (c < 0x20 || c == 0x7f) {
      out += "\\u00";
      out += kDigits[c >> 4];
      out += kDigits[c & 0xf];
    } else if (c < 0x80) {
      out += static_cast<char>(c);
    } else {
      // A whole UTF-8 sequence, or this one byte as U+0080..U+00FF.
      if (const size_t length = utf8_length(text.substr(i)); length != 0) {
        out += text.substr(i, length);
        i += length - 1;
      } else {
        out += static_cast<char>(0xc0 | c >> 6);
        out += static_cast<char>(0x80 | (c & 0x3f));
      }
    }
  }
  out += '"';
}

// A JSON object, built member by member.
class JsonObject {
 public:
  JsonObject& add(std::string_view key, std::string_view text) {
    append_json_string(key_(key), text);
    return *this;
  }
  JsonObject& add(std::string_view key, uint64_t value) { return add_json(key, std::to_string(value)); }
  // `json` is already JSON: a number, an object.
  JsonObject& add_json(std::string_view key, std::string_view json) {
    key_(key) += json;
    return *this;
  }
  std::string str() const { return out_ + '}'; }

 private:
  std::string& key_(std::string_view key) {
    if (out_.size() > 1) out_ += ',';
    append_json_string(out_, key);
    out_ += ':';
    return out_;
  }

  std::string out_ = "{";
};

bool parse_whole(std::string_view text, uint32_t& value) {
  const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), value);
  return error == std::errc() && end == text.data() + text.size() && !text.empty();
}

// The record's value, and its text as fields (see format_json).
void add_fields(JsonObject& out, const TraceRecord& r) {
  out.add("value", r.value);
  if (!r.text) return;
  const std::string_view text = *r.text;
  const std::string& label = r.label;
  const size_t space = text.find(' ');
  if ((label == "tx" || label == "rx") && space != std::string_view::npos) {
    out.add("device", text.substr(0, space)).add("hex", text.substr(space + 1));
    return;
  }
  if ((label == "mem" && space != std::string_view::npos) || (label == "erase" && space == std::string_view::npos)) {
    const std::string_view where = text.substr(0, space);  // device+offset
    const size_t plus = where.rfind('+');
    uint32_t offset = 0;
    if (plus != std::string_view::npos && parse_whole(where.substr(plus + 1), offset)) {
      out.add("device", where.substr(0, plus)).add("offset", offset);
      if (label == "mem") out.add("hex", text.substr(space + 1));
      return;
    }
  }
  if (label == "drop" || label == "watchdog") {
    out.add("device", text);
    return;
  }
  if (label == "reg" || label == "set") {  // device[reg]=value
    const size_t open = text.find('[');
    const size_t close = text.find("]=");
    uint32_t reg = 0;
    if (open != std::string_view::npos && close != std::string_view::npos && close > open &&
        parse_whole(text.substr(open + 1, close - open - 1), reg)) {
      out.add("device", text.substr(0, open)).add("register", reg);
      return;
    }
  }
  out.add("text", text);
}

// Microseconds, to the nanosecond: the trace-event format's unit.
std::string microseconds_text(Time ns) { return std::format("{}.{:03}", ns / 1000, ns % 1000); }

JsonObject metadata(std::optional<uint32_t> tid, std::string_view name, const JsonObject& args) {
  JsonObject event;
  event.add("ph", "M").add("pid", 1);
  if (tid) event.add("tid", *tid);
  event.add("name", name).add_json("args", args.str());
  return event;
}

}  // namespace

std::string to_utf8(std::string_view text) {
  std::string out;
  for (size_t i = 0; i < text.size(); ++i) {
    const auto c = static_cast<unsigned char>(text[i]);
    const size_t length = c < 0x80 ? 1 : utf8_length(text.substr(i));
    if (length != 0) {
      out += text.substr(i, length);
      i += length - 1;
    } else {
      out += static_cast<char>(0xc0 | c >> 6);
      out += static_cast<char>(0x80 | (c & 0x3f));
    }
  }
  return out;
}

std::string xml_escaped(std::string_view text, bool attribute) {
  const std::string valid = to_utf8(text);
  std::string out;
  for (size_t i = 0; i < valid.size();) {
    // One code point: to_utf8() leaves only whole, valid sequences.
    const auto c = static_cast<unsigned char>(valid[i]);
    const size_t length = c < 0x80 ? 1 : c < 0xe0 ? 2 : c < 0xf0 ? 3 : 4;
    char32_t code = length == 1 ? c : c & (0x7f >> length);
    for (size_t k = 1; k < length; ++k) code = code << 6 | (static_cast<unsigned char>(valid[i + k]) & 0x3f);
    switch (code) {
      case '&': out += "&amp;"; break;
      case '<': out += "&lt;"; break;
      case '>': out += "&gt;"; break;
      case '"': out += "&quot;"; break;
      case '\n': out += attribute ? "&#10;" : "\n"; break;
      case '\t': out += attribute ? "&#9;" : "\t"; break;
      case '\r': out += "&#13;"; break;
      default:
        // XML 1.0's characters: no other C0 controls, and not U+FFFE or
        // U+FFFF (surrogates aren't valid UTF-8, so to_utf8() has none).
        if (code < 0x20 || code == 0xfffe || code == 0xffff) {
          out += '?';
        } else {
          out.append(valid, i, length);
        }
    }
    i += length;
  }
  return out;
}

bool TraceFilter::keeps(const TraceRecord& r) const {
  if (!machines.empty() && std::ranges::find(machines, r.machine) == machines.end()) return false;
  const auto matches = [&r](const std::string& pattern) { return label_matches(pattern, r.label); };
  if (!labels.empty() && std::ranges::none_of(labels, matches)) return false;
  return std::ranges::none_of(excluded_labels, matches);
}

std::string format_json(const TraceRecord& r, std::string_view machine) {
  JsonObject out;
  out.add("time_ns", r.at).add("machine", machine).add("label", r.label);
  add_fields(out, r);
  return out.str();
}

PerfettoTrace::PerfettoTrace(std::string title, std::vector<std::string> machines)
    : title_(std::move(title)), machines_(std::move(machines)) {}

// Metadata first: the process is the scenario, each machine a thread.
std::string PerfettoTrace::begin() const {
  std::string out = "[\n" + metadata(std::nullopt, "process_name", JsonObject().add("name", title_)).str();
  for (uint32_t id = 0; id < machines_.size(); ++id) {
    out += ",\n" + metadata(id + 1, "thread_name", JsonObject().add("name", machines_[id])).str();
    out += ",\n" + metadata(id + 1, "thread_sort_index", JsonObject().add("sort_index", id)).str();
  }
  return out;
}

std::string PerfettoTrace::event(const TraceRecord& r) const {
  const std::string ts = microseconds_text(r.at);
  JsonObject args;
  add_fields(args, r);
  JsonObject instant;
  instant.add("ph", "i").add("s", "t").add("pid", 1).add("tid", r.machine + 1).add_json("ts", ts);
  std::string out = ",\n" + instant.add("name", r.label).add_json("args", args.str()).str();
  // A register's counter: "<machine> <device>" with a series per register,
  // which Perfetto shows as "<machine> <device> [<reg>]".
  const size_t open = r.text ? r.text->find('[') : std::string::npos;
  const size_t close = r.text ? r.text->find("]=") : std::string::npos;
  if ((r.label == "reg" || r.label == "set") && open != std::string::npos && close != std::string::npos) {
    const std::string machine = r.machine < machines_.size() ? machines_[r.machine] : "m" + std::to_string(r.machine);
    JsonObject counter;
    counter.add("ph", "C").add("pid", 1).add_json("ts", ts).add("name", machine + " " + r.text->substr(0, open));
    const std::string series = r.text->substr(open, close + 1 - open);
    out += ",\n" + counter.add_json("args", JsonObject().add(series, r.value).str()).str();
  }
  return out;
}

std::string format(const TraceRecord& r) {
  if (!r.text) return std::format("{} m{} {} {}", r.at, r.machine, r.label, r.value);
  if (r.text->empty()) return std::format("{} m{} {}", r.at, r.machine, r.label);
  return std::format("{} m{} {} {}", r.at, r.machine, r.label, *r.text);
}

std::string Trace::format(const TraceFilter& filter) const {
  std::string out;
  for (const TraceRecord& r : records_) {
    if (!filter.keeps(r)) continue;
    out += costar::format(r);
    out += '\n';
  }
  return out;
}

}  // namespace costar
