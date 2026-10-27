# ndk-build shim for libretro's android-jni CI template.
#
# The core is built by cargo through Makefile.libretro; ndk-build only packages the result as
# libs/<abi>/libretro.so.

LOCAL_PATH := $(call my-dir)
CORE_DIR := $(LOCAL_PATH)/..

JGENESIS_PREBUILT := prebuilt/$(TARGET_ARCH_ABI)/libretro.so

# Clear MAKEFLAGS so ndk-build's command-line variables don't leak into the nested make
JGENESIS_BUILD_STATUS := $(shell \
	MAKEFLAGS= MAKELEVEL= make -C $(CORE_DIR) -f Makefile.libretro platform=android ABI=$(TARGET_ARCH_ABI) \
		NDK_ROOT=$(NDK_ROOT) ANDROID_API=$(patsubst android-%,%,$(APP_PLATFORM)) >&2 && \
	mkdir -p $(LOCAL_PATH)/$(dir $(JGENESIS_PREBUILT)) && \
	cp $(CORE_DIR)/jgenesis_libretro_android.so $(LOCAL_PATH)/$(JGENESIS_PREBUILT) && \
	echo ok)

ifneq ($(JGENESIS_BUILD_STATUS),ok)
  $(error cargo build of jgenesis for $(TARGET_ARCH_ABI) failed)
endif

include $(CLEAR_VARS)
LOCAL_MODULE := retro
LOCAL_SRC_FILES := $(JGENESIS_PREBUILT)
include $(PREBUILT_SHARED_LIBRARY)
