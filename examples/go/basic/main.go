package main

import (
	"fmt"
	"html"
	"net/http"
	"os"

	"tako.sh"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/", handleRoot)
	mux.HandleFunc("/health", handleHealth)

	if err := tako.ListenAndServe(mux); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}

func handleRoot(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path != "/" {
		http.NotFound(w, r)
		return
	}

	pid := os.Getpid()
	name := os.Getenv("APP_NAME")
	if name == "" {
		name = "World"
	}

	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	fmt.Fprintf(w, `<!doctype html>
<html>
<body>
  <h1>Hello, %s!</h1>
  <p>Go example for Tako</p>
  <p>PID: %d</p>
  <p>Secret check: %s</p>
</body>
</html>`, html.EscapeString(name), pid, secretStatus())
}

func handleHealth(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	fmt.Fprint(w, `{"ok":true}`)
}

func secretStatus() string {
	if s := Secrets.ExampleSecret(); s != "" {
		return "present"
	}
	return "not set"
}
