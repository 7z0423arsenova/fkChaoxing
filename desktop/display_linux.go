//go:build linux

package main

import (
	"fmt"
	"os"
	"syscall"
)

func configureDisplayBackend() error {
	if !shouldUseX11(os.Getenv("GDK_BACKEND"), os.Getenv("DISPLAY"), os.Getenv("WAYLAND_DISPLAY")) {
		return nil
	}
	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("确定桌面程序路径失败：%w", err)
	}
	// GTK reads its display backend from the process environment at startup.
	// Re-exec once so a mixed Wayland/X11 session can use its X11 socket.
	return syscall.Exec(exe, os.Args, append(os.Environ(), "GDK_BACKEND=x11"))
}
