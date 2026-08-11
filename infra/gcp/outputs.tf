output "deployment_enabled" {
  description = "True only after both the opt-in and acknowledgement are supplied."
  value       = local.enabled
}

output "deployment_stage" {
  description = "disabled, bootstrap, or runtime according to the reviewed inputs."
  value       = !local.enabled ? "disabled" : local.runtime_enabled ? "runtime" : "bootstrap"
}

output "planned_bucket" {
  description = "Bucket that will be created only when deployment is explicitly enabled."
  value       = local.enabled ? var.bucket_name : null
}

output "cloud_run_service" {
  description = "Private Cloud Run service name after a real apply."
  value       = local.runtime_enabled ? google_cloud_run_v2_service.runtime[0].name : null
}

output "artifact_registry_repository" {
  description = "Regional Docker repository created during the bootstrap stage."
  value       = local.enabled ? google_artifact_registry_repository.runtime[0].name : null
}

output "cloud_build_service_account" {
  description = "Least-privilege service account used for the reviewed image build."
  value       = local.enabled ? google_service_account.builder[0].email : null
}
