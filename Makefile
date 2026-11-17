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
CPPFLAGS += -Icore/include -Itests

# make SANITIZE=1 -> ASan + UBSan (PLAN.md §10: every PR, lite).
ifdef SANITIZE
CXXFLAGS += -fsanitize=address,undefined -fno-omit-frame-pointer
endif

BUILD := build

CORE_SRC := $(wildcard core/src/*.cpp)
CLI_SRC  := $(wildcard cli/src/*.cpp)
TEST_SRC := $(wildcard tests/*.cpp tests/conformance/*.cpp)

CORE_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(CORE_SRC))
CLI_OBJ  := $(patsubst %.cpp,$(BUILD)/%.o,$(CLI_SRC))
TEST_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(TEST_SRC))

.PHONY: all test clean

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

$(BUILD)/editor: $(CLI_OBJ) $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(CLI_OBJ) $(BUILD)/libcore.a

$(BUILD)/editor_tests: $(TEST_OBJ) $(CORE_OBJ)
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(TEST_OBJ) $(CORE_OBJ)

$(BUILD)/%.o: %.cpp
	@mkdir -p $(@D)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -c $< -o $@

test: $(BUILD)/editor_tests
	$(BUILD)/editor_tests

clean:
	rm -rf $(BUILD)
