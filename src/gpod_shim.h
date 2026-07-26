#ifndef COPYPOD_GPOD_SHIM_H
#define COPYPOD_GPOD_SHIM_H

#include <stddef.h>
#include <stdint.h>

typedef struct CpDb CpDb;
typedef void CpTrack;
typedef void CpTrackNode;

typedef struct {
    const char *title;
    const char *album;
    const char *artist;
    const char *album_artist;
    const char *genre;
    const char *comment;
    uint64_t size;
    int64_t modified_at;
    uint32_t duration_ms;
    uint32_t bitrate_kbps;
    uint32_t sample_rate_hz;
    uint32_t year;
    uint32_t track_number;
    uint32_t track_total;
    uint32_t disc_number;
    uint32_t disc_total;
} CpMetadata;

CpDb *cp_db_open(const char *mountpoint, char **error);
void cp_db_free(CpDb *db);

char *cp_db_description(const CpDb *db);
char *cp_db_database_path(const CpDb *db);
size_t cp_db_track_count(const CpDb *db);
CpTrackNode *cp_db_tracks(const CpDb *db);
CpTrackNode *cp_track_node_next(const CpTrackNode *node);
CpTrack *cp_track_node_track(const CpTrackNode *node);

char *cp_track_path(const CpTrack *track);
char *cp_track_title(const CpTrack *track);
char *cp_track_artist(const CpTrack *track);

int cp_db_remove_track(CpDb *db, CpTrack *track, char **error);
int cp_db_add_track(CpDb *db, const char *source_path,
                    const CpMetadata *metadata, char **copied_path,
                    char **error);
int cp_db_write(CpDb *db, char **error);

void cp_string_free(char *value);

#endif
