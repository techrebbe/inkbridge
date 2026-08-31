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

output "cloud_build_source_bucket" {
  description = "Dedicated transient source bucket readable by the image-builder account."
  value       = local.enabled ? google_storage_bucket.build_source[0].name : null
}

output "folder_transport_service_account" {
  description = "Read-only bucket identity with create-only writes restricted to BOOX_Folder/ and Supernote_Folder/."
  value       = local.enabled ? google_service_account.folder_transport[0].email : null
}

output "drive_runtime_stage" {
  description = "disabled, bootstrap, dry-run, or apply according to the guarded Drive job inputs."
  value       = local.drive_runtime_stage
}

output "drive_runtime_job" {
  description = "Private, manually invoked Drive gateway Cloud Run Job after a real apply."
  value       = local.drive_runtime_enabled ? google_cloud_run_v2_job.drive_runtime[0].name : null
}

output "drive_runtime_service_account" {
  description = "Least-privilege identity used only by the Drive gateway job."
  value       = local.enabled ? google_service_account.drive_runtime[0].email : null
}

output "drive_runtime_secret_ids" {
  description = "Secret Manager container IDs. Secret versions are deliberately outside Terraform."
  value = local.enabled ? {
    oauth_client  = google_secret_manager_secret.drive_oauth_client[0].secret_id
    refresh_token = google_secret_manager_secret.drive_refresh_token[0].secret_id
  } : null
}
