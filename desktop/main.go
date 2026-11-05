package main

import (
	_ "embed"
	"log"
	"os"
	"path/filepath"

	"github.com/egoist/mygo"
)

//go:embed page.html
var page string

func main() {
	mygo.App.SetName("fkChaoxing")
	if err := preserveUserData(); err != nil {
		log.Fatal(err)
	}
	if err := configureDisplayBackend(); err != nil {
		log.Fatal(err)
	}
	mygo.Bind(&Bridge{})
	mygo.App.WhenReady(func() {
		window := mygo.NewWindow(mygo.WindowOptions{
			Title:  "fkChaoxing",
			Width:  820,
			Height: 680,
		})
		window.Page().LoadHTML(page, "")
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}

// Keep existing accounts and cookies available after changing the app name.
func preserveUserData() error {
	dir, err := os.UserConfigDir()
	if err != nil {
		return err
	}
	return preserveUserDataAt(dir)
}

func preserveUserDataAt(dir string) error {
	current := filepath.Join(dir, "fkChaoxing")
	if _, err := os.Stat(current); err == nil {
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	legacy := filepath.Join(dir, "Chaoxing")
	if info, err := os.Stat(legacy); err == nil && info.IsDir() {
		mygo.App.SetPath(mygo.PathUserData, legacy)
	} else if err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func shouldUseX11(backend, display, waylandDisplay string) bool {
	return backend == "" && display != "" && waylandDisplay != ""
}
