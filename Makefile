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
CPPFLAGS += -Icore/include -Icore/src -Icli/src -Iviewcore/include -Iviewcore/src -Iplatform/macos -Itests

# make SANITIZE=1 -> ASan + UBSan (PLAN.md §10: every PR, lite).
ifdef SANITIZE
CXXFLAGS += -fsanitize=address,undefined -fno-omit-frame-pointer
endif

# Keep sanitizer objects in a separate tree so toggling SANITIZE never mixes
# instrumented and plain objects in one link.
BUILD := build$(if $(SANITIZE),-asan,)

CORE_SRC := $(wildcard core/src/*.cpp)
PLATFORM_SRC := $(wildcard platform/macos/*.cpp)
PLATFORM_GUI_SRC := $(wildcard platform/macos/*.mm)
APP_SRC := $(wildcard platform/macos/app/*.mm)
VIEWCORE_SRC := $(wildcard viewcore/src/*.cpp)
CLI_SRC  := $(wildcard cli/src/*.cpp)
BENCH_SRC := $(wildcard bench/*.cpp)
DEMO_SRC := $(wildcard demo/*.cpp)
TEST_SRC := $(wildcard tests/*.cpp tests/conformance/*.cpp)

CORE_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(CORE_SRC))
PLATFORM_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(PLATFORM_SRC))
PLATFORM_GUI_OBJ := $(patsubst %.mm,$(BUILD)/%.o,$(PLATFORM_GUI_SRC))
APP_OBJ := $(patsubst %.mm,$(BUILD)/%.o,$(APP_SRC))
VIEWCORE_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(VIEWCORE_SRC))
CLI_OBJ  := $(patsubst %.cpp,$(BUILD)/%.o,$(CLI_SRC))
BENCH_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(BENCH_SRC))
DEMO_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(DEMO_SRC))
TEST_OBJ := $(patsubst %.cpp,$(BUILD)/%.o,$(TEST_SRC))

# CoreText/CoreGraphics backends (platform/macos/*.mm). The app shell lives in
# platform/macos/app/ and is built only by `make app`.
FRAMEWORKS := -framework Foundation -framework CoreText -framework CoreGraphics
APP_FRAMEWORKS := $(FRAMEWORKS) -framework AppKit

.PHONY: all test golden fuzz bench demo app clean

all: $(BUILD)/editor_tests
ifneq ($(CORE_SRC),)
all: $(BUILD)/libcore.a
endif
ifneq ($(VIEWCORE_SRC),)
all: $(BUILD)/libviewcore.a
endif
ifneq ($(CLI_SRC),)
all: $(BUILD)/editor
endif

$(BUILD)/libcore.a: $(CORE_OBJ)
	@mkdir -p $(@D)
	ar rcs $@ $^

$(BUILD)/libviewcore.a: $(VIEWCORE_OBJ)
	@mkdir -p $(@D)
	ar rcs $@ $^

$(BUILD)/editor: $(CLI_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(CLI_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a

$(BUILD)/editor_tests: $(TEST_OBJ) $(PLATFORM_OBJ) $(PLATFORM_GUI_OBJ) $(VIEWCORE_OBJ) $(CORE_OBJ)
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(TEST_OBJ) $(PLATFORM_OBJ) $(PLATFORM_GUI_OBJ) $(VIEWCORE_OBJ) $(CORE_OBJ) $(FRAMEWORKS)

$(BUILD)/%.o: %.cpp
	@mkdir -p $(@D)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -MMD -MP -c $< -o $@

$(BUILD)/%.o: %.mm
	@mkdir -p $(@D)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) -x objective-c++ -fobjc-arc -MMD -MP -c $< -o $@

-include $(CORE_OBJ:.o=.d) $(PLATFORM_OBJ:.o=.d) $(PLATFORM_GUI_OBJ:.o=.d) $(APP_OBJ:.o=.d) $(VIEWCORE_OBJ:.o=.d) $(CLI_OBJ:.o=.d) $(BENCH_OBJ:.o=.d) $(DEMO_OBJ:.o=.d) $(TEST_OBJ:.o=.d)

test: $(BUILD)/editor_tests
	$(BUILD)/editor_tests

golden: $(BUILD)/editor
	python3 tests/cli_golden.py $(BUILD)/editor

# Deterministic fuzzing. The C++ fuzzers (UTF-8, JSON reader/writer, line index)
# run under `test`; this drives the CLI argv/batch fuzzer (§9).
fuzz: $(BUILD)/editor
	python3 tests/cli_fuzz.py $(BUILD)/editor

$(BUILD)/bench_edit: $(BENCH_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(BENCH_OBJ) $(PLATFORM_OBJ) $(BUILD)/libcore.a

bench: $(BUILD)/bench_edit
	$(BUILD)/bench_edit

# The 5 GB read-only view demo / footprint gate (M0.5).
$(BUILD)/view_demo: $(DEMO_OBJ) $(PLATFORM_OBJ) $(BUILD)/libviewcore.a $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(DEMO_OBJ) $(PLATFORM_OBJ) $(BUILD)/libviewcore.a $(BUILD)/libcore.a

demo: $(BUILD)/view_demo
	$(BUILD)/view_demo

# The M1 AppKit read-only view (platform/macos/app/gui_main.mm).
$(BUILD)/editor_gui: $(APP_OBJ) $(PLATFORM_OBJ) $(PLATFORM_GUI_OBJ) $(BUILD)/libviewcore.a $(BUILD)/libcore.a
	@mkdir -p $(@D)
	$(CXX) $(CXXFLAGS) -o $@ $(APP_OBJ) $(PLATFORM_OBJ) $(PLATFORM_GUI_OBJ) $(BUILD)/libviewcore.a $(BUILD)/libcore.a $(APP_FRAMEWORKS)

# The launchable .app bundle (notarization/packaging proper is M3.5).
APP_BUNDLE := $(BUILD)/editor.app
APP_EXEC := $(APP_BUNDLE)/Contents/MacOS/editor

$(APP_EXEC): $(BUILD)/editor_gui platform/macos/app/Info.plist
	@mkdir -p $(APP_BUNDLE)/Contents/MacOS
	cp $(BUILD)/editor_gui $@
	cp platform/macos/app/Info.plist $(APP_BUNDLE)/Contents/Info.plist

app: $(APP_EXEC)

bundle: $(APP_EXEC)

clean:
	rm -rf build build-asan
