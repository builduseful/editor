# editor — Pre-M0.0 build.
#
# Plain Apple clang + make: both ship with the Xcode Command Line Tools, so
# there is nothing to install and no third-party tooling. PLAN.md §10 names a
# CMake graph (Xcode generator) for the full app/`.app`/notarization build; that
# wraps these same sources when the GUI work starts (M0.5/M3.5). Sources are
# discovered by wildcard, so adding core/src, cli/src, etc. needs no edits here.

CXX      ?= clang++
CXXFLAGS ?= -std=c++20 -Wall -Wextra -Wconversion -O2 -g \
            -mmacosx-version-min=15.0 -arch arm64
CPPFLAGS += -Icore/include -Icore/src -Icli/src -Itests

# make SANITIZE=1 -> ASan + UBSan (PLAN.md §10: every PR, lite).
ifdef SANITIZE
CXXFLAGS += -fsanitize=address,undefined -fno-omit-frame-pointer
endif

# Keep sanitizer objects in a separate tree so toggling SANITIZE never mixes
# instrumented and plain objects in one link.
BUILD := build$(if $(SANITIZE),-asan,)

CORE_SRC := $(wildcard core/src/*.cpp)
PLATFORM_SRC := $(wildcard platform/macos/*.cpp)
CLI_SRC  := $(wildcard cli/src/*.cpp)
BENCH_SRC := $(wildcard bench/*.cpp)
TEST_SRC := $(wildcard tests/*.cpp tests/conformance/*.cpp)

CORE_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(CORE_SRC))
PLATFORM_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(PLATFORM_SRC))
CLI_OBJ  := $(patsubst %.cpp,$(BUILD)/%.o,$(CLI_SRC))
BENCH_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(BENCH_SRC))
TEST_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(TEST_SRC))

.PHONY: all test golden bench clean

all: $(BUILD)/editor_tests
ifneq ($(CORE_SRC),)
all: $(BUILD)/libcore.a
endif
ifneq ($(CLI_SRC),)
all: $(BUILD)/editor
endif

$(BUILD)/libcore.a: $(CORE_OBJ)
	@mkdir -p $(@D)
	ar rcs $@ $^

$(BUILD)/editor: $(CLI_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(CLI_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a

$(BUILD)/editor_tests: $(TEST_OBJ) $(PLATFORM_OBJ) $(CORE_OBJ)
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(TEST_OBJ) $(PLATFORM_OBJ) $(CORE_OBJ)

$(BUILD)/%.o: %.cpp
	@mkdir -p $(@D)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -MMD -MP -c $< -o $@

-include $(CORE_OBJ:.o=.d) $(PLATFORM_OBJ:.o=.d) $(CLI_OBJ:.o=.d) $(BENCH_OBJ:.o=.d) $(TEST_OBJ:.o=.d)

test: $(BUILD)/editor_tests
	$(BUILD)/editor_tests

golden: $(BUILD)/editor
	python3 tests/cli_golden.py $(BUILD)/editor

$(BUILD)/bench_edit: $(BENCH_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(BENCH_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a

bench: $(BUILD)/bench_edit
	$(BUILD)/bench_edit

clean:
	rm -rf build build-asan
