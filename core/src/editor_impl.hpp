// core/src/editor_impl.hpp — the §4 Editor implementation (M0a).
#pragma once

#include <cstdint>
#include <span>
#include <string>
#include <vector>

#include "add_buffer.hpp"
#include "editor.hpp"
#include "fd_source.hpp"
#include "line_index.hpp"
#include "owned_fd.hpp"
#include "piece_table.hpp"

namespace ed {

class EditorImpl final : public Editor {
 public:
  explicit EditorImpl(FileSystem* fs);
  ~EditorImpl() override;

  Status open(std::string_view path_utf8) override;
  Status create(std::string_view path_utf8) override;
  Status info(Info* out) const override;
  Rev rev() const override;
  Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                    size_t* used, int64_t* out_n) const override;
  Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                    size_t* used) const override;
  Status apply(std::span<const Edit> edits) override;
  Status undo() override;
  Status redo() override;
  Status save(const Rev* expected_rev, bool in_place, SaveInfo* out) override;
  Status save_as(std::string_view path_utf8, const Rev* expected_rev,
                 bool in_place, SaveInfo* out) override;
  Status reload() override;
  Status check_identity() override;
  uint64_t on_change(ChangeFn fn, void* ud) override;
  void off_change(uint64_t id) override;
  Status drain() override;
  std::string_view errmsg() const override;

 private:
  Status fail(Status s, std::string msg) const;
  Status install_fd(int fd, const Stat& st, const std::string& path);
  Status ensure_index() const;
  Status ensure_eol() const;
  Eol detect_eol() const;
  bool final_newline() const;
  Status logical_offset(int64_t line, int64_t col, uint64_t* out) const;
  Status line_content_len(int64_t line, uint64_t* out) const;
  Status line_of_byte(uint64_t off, int64_t* out) const;
  void emit(const Change& c);
  Status do_save(const Rev* expected_rev, bool in_place, SaveInfo* out);
  Status flatten_after_save(uint64_t new_size);

  FileSystem* fs_ = nullptr;
  OwnedFd base_fd_;
  FdSource base_src_;
  OffsetSource logical_src_;
  uint64_t base_size_ = 0;   // on-disk size (BOM included)
  uint64_t bom_len_ = 0;
  std::string path_;
  std::string dir_;
  AddBuffer add_;
  PieceTable pieces_;
  mutable LineIndex index_;
  mutable bool index_built_ = false;
  mutable Eol eol_ = Eol::Lf;
  mutable bool eol_known_ = false;
  bool dirty_ = false;
  bool opened_ = false;
  bool needs_reload_ = false;
  Rev rev_;
  mutable std::string errmsg_;

  struct Cb {
    uint64_t id;
    ChangeFn fn;
    void* ud;
  };
  std::vector<Cb> cbs_;
  std::vector<Change> pending_;
  uint64_t next_cb_id_ = 1;
};

}  // namespace ed
