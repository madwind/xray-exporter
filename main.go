package main

import (
	"context"
	"log"
	"net/http"
	"strings"
	"time"

	statsService "github.com/xtls/xray-core/app/stats/command"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
)

var (
	xrayTraffic = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_traffic_bytes",
			Help: "Xray traffic statistics (user/inbound/outbound)",
		},
		[]string{"type", "name", "direction"},
	)
	xrayUserIPOnline = prometheus.NewGaugeVec(
		prometheus.GaugeOpts{
			Name: "xray_user_ip_online",
			Help: "User online status per IP (1=online)",
		},
		[]string{"name", "ip"},
	)
	xrayUp = prometheus.NewGauge(
		prometheus.GaugeOpts{
			Name: "xray_up",
			Help: "Whether Xray is reachable",
		},
	)
)

func main() {
	reg := prometheus.NewRegistry()
	reg.MustRegister(xrayTraffic)
	reg.MustRegister(xrayUserIPOnline)
	reg.MustRegister(xrayUp)

	conn, err := grpc.NewClient("127.0.0.1:22222", grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		log.Fatal("Connect to Xray failed:", err)
	}
	defer func(conn *grpc.ClientConn) {
		err := conn.Close()
		if err != nil {

		}
	}(conn)
	client := statsService.NewStatsServiceClient(conn)

	go func() {
		for {
			if err := scrapeTraffic(client); err != nil {
				xrayUp.Set(0)
				log.Println("scrapeTraffic error:", err)
			} else {
				xrayUp.Set(1)
			}
			if err := scrapeUserIPOnline(client); err != nil {
				log.Println("scrapeUserIPOnline error:", err)
			}
			time.Sleep(5 * time.Second)
		}
	}()

	http.Handle("/metrics", promhttp.HandlerFor(reg, promhttp.HandlerOpts{}))
	log.Println("Exporter listening on :9550/metrics")
	log.Fatal(http.ListenAndServe(":9550", nil))
}

func scrapeTraffic(c statsService.StatsServiceClient) error {
	resp, err := c.QueryStats(context.Background(), &statsService.QueryStatsRequest{
		Pattern: "",
		Reset_:  false,
	})
	if err != nil {
		return err
	}
	for _, stat := range resp.Stat {
		parseAndSetTraffic(stat.Name, stat.Value)
	}
	return nil
}

func parseAndSetTraffic(name string, value int64) {
	parts := strings.Split(name, ">>>")
	if len(parts) < 4 {
		return
	}
	xrayTraffic.WithLabelValues(parts[0], parts[1], parts[3]).Set(float64(value))
}

func scrapeUserIPOnline(c statsService.StatsServiceClient) error {
	resp, err := c.QueryStats(context.Background(), &statsService.QueryStatsRequest{
		Pattern: "user>>>",
		Reset_:  false,
	})
	if err != nil {
		return err
	}

	xrayUserIPOnline.Reset()
	for _, stat := range resp.Stat {
		user := strings.Split(stat.Name, ">>>")[1]
		ipResp, err := c.GetStatsOnlineIpList(context.Background(), &statsService.GetStatsRequest{
			Name: "user>>>" + user,
		})
		if err != nil {
			continue
		}
		for ip := range ipResp.Ips {
			xrayUserIPOnline.WithLabelValues(user, ip).Set(1)
		}
	}
	return nil
}
