package main

import (
	"fmt"
	"net/http"

	"tako.sh"
)

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprint(w, "<!doctype html><html><body><h1>Tako Go app</h1></body></html>")
	})

	if err := tako.ListenAndServe(mux); err != nil {
		fmt.Printf("server error: %v\n", err)
	}
}
