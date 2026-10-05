module github.com/open-console/ocg-cpa-host

go 1.26.0

require (
	github.com/gin-gonic/gin v1.10.1
	github.com/router-for-me/CLIProxyAPI/v8 v8.0.10
	gopkg.in/yaml.v3 v3.0.1
)

replace github.com/router-for-me/CLIProxyAPI/v8 => ../patched-cpa
