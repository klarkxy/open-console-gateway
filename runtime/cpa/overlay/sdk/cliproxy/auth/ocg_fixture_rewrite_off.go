//go:build !ocg_native_loopback_fixture

package auth

import "net/http"

func applyAdmittedNativeFixture(*generationFacts, *http.Request) error {
	return nil
}
