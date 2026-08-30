#!/usr/bin/env python3
"""
POC S2.1 (macOS / Linux)：无障碍树读取可用性验证

Windows 请用同目录 poc_a11y_windows.ps1（基于 .NET UIAutomation，已实测可用）。

用法：
  macOS:  python3 poc_a11y_tree.py --max-depth 6
  Linux:  python3 poc_a11y_tree.py --max-depth 6
前置（首次）：
  macOS:  pip install pyobjc-framework-Quartz pyobjc-framework-ApplicationServices
          并在「系统设置 → 隐私与安全性 → 辅助功能」授权终端
  Linux:  pip install pyatspi  （或 apt install python3-pyatspi）
通过标准：能打印焦点窗口的结构化树摘要（role/name/bbox），节点数 > 0。
"""

import argparse
import platform
import sys

MAX_DEPTH = 6


def run_macos():
    try:
        from ApplicationServices import (
            AXUIElementCreateSystemWide,
            AXUIElementCopyAttributeValue,
            kAXFocusedApplicationAttribute,
            kAXFocusedWindowAttribute,
            kAXRoleAttribute,
            kAXTitleAttribute,
            kAXValueAttribute,
            kAXChildrenAttribute,
            kAXPositionAttribute,
            kAXSizeAttribute,
        )
    except ImportError:
        print("[X] 需要 pyobjc：pip install pyobjc-framework-Quartz pyobjc-framework-ApplicationServices")
        sys.exit(2)

    def attr(el, key):
        err, val = AXUIElementCopyAttributeValue(el, key, None)
        return val if not err else None

    syswide = AXUIElementCreateSystemWide()
    app = attr(syswide, kAXFocusedApplicationAttribute)
    if not app:
        print("[FAIL] 无法获取焦点应用（请先授予「辅助功能」权限）")
        sys.exit(1)
    win = attr(app, kAXFocusedWindowAttribute)
    root = win or app

    print("根: role=%r title=%r" % (attr(root, kAXRoleAttribute), attr(root, kAXTitleAttribute)))
    count = [0]

    def walk(el, depth):
        if depth > MAX_DEPTH:
            return
        count[0] += 1
        role = attr(el, kAXRoleAttribute) or ""
        title = attr(el, kAXTitleAttribute) or ""
        value = attr(el, kAXValueAttribute) or ""
        pos = attr(el, kAXPositionAttribute) or (0, 0)
        size = attr(el, kAXSizeAttribute) or (0, 0)
        print("  " * depth + "[%s] title=%r value=%r bbox=(%s,%s,%s,%s)"
              % (role, title, value, pos[0], pos[1], size[0], size[1]))
        children = attr(el, kAXChildrenAttribute) or []
        for c in children:
            walk(c, depth + 1)

    walk(root, 0)
    print("节点总数: %d" % count[0])
    print("[PASS]" if count[0] > 0 else "[FAIL]")
    sys.exit(0 if count[0] > 0 else 1)


def run_linux():
    try:
        import pyatspi
    except ImportError:
        print("[X] 需要 pyatspi：pip install pyatspi（或 apt install python3-pyatspi）")
        sys.exit(2)

    desktop = pyatspi.Registry.getDesktop(0)
    count = [0]

    def walk(acc, depth):
        if depth > MAX_DEPTH:
            return
        count[0] += 1
        try:
            role = acc.getRoleName()
        except Exception:
            role = "?"
        name = getattr(acc, "name", "") or ""
        try:
            pos = acc.getPosition(pyatspi.DESKTOP_COORDS)
            size = acc.getSize()
        except Exception:
            pos, size = (0, 0), (0, 0)
        print("  " * depth + "[%s] name=%r bbox=(%s,%s,%s,%s)"
              % (role, name, pos[0], pos[1], size[0], size[1]))
        for c in acc:
            walk(c, depth + 1)

    walk(desktop, 0)
    print("节点总数: %d" % count[0])
    print("[PASS]" if count[0] > 0 else "[FAIL]")
    sys.exit(0 if count[0] > 0 else 1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--max-depth", type=int, default=MAX_DEPTH)
    a = ap.parse_args()
    global MAX_DEPTH
    MAX_DEPTH = a.max_depth

    s = platform.system()
    if s == "Darwin":
        run_macos()
    elif s == "Linux":
        run_linux()
    else:
        print("[INFO] %s 平台请使用 poc_a11y_windows.ps1" % s)
        sys.exit(0)


if __name__ == "__main__":
    main()
